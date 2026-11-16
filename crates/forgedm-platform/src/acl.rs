//! Data-directory owner and DACL check (THREAT_MODEL FS-02, S1.14).
//!
//! An explicit `--data-dir` is only trusted when the current user owns it and no allow ACE
//! gives write access to anyone but the current user, SYSTEM or Administrators. Anything the
//! check cannot interpret (a NULL DACL, an object ACE) fails closed. A directory the agent
//! creates itself gets an explicit protected DACL ([`create_private_dir`]); the default
//! `%LOCALAPPDATA%\ForgeDM` keeps its inherited profile ACL, which already restricts the
//! profile to those three principals.
#![allow(unsafe_code)] // Win32 security FFI; every block carries a SAFETY comment.

use std::fs::OpenOptions;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, ERROR_SUCCESS, HANDLE, HLOCAL};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SE_FILE_OBJECT,
};
use windows::Win32::Security::{
    ACL, ACL_SIZE_INFORMATION, AclSizeInformation, DACL_SECURITY_INFORMATION, GetAce,
    GetAclInformation, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
};
use windows::Win32::Storage::FileSystem::CreateDirectoryW;
use windows::core::{Owned, PCWSTR};

use crate::error::{PlatformError, win32_err};
use crate::sid::Sid;
use crate::wide::{to_wide_nul, verbatim_wide};

const SDDL_REVISION_1: u32 = 1;
const READ_CONTROL: u32 = 0x0002_0000;
const FILE_SHARE_ALL: u32 = 0x7;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
/// `ACCESS_ALLOWED_ACE_TYPE`.
const ACE_TYPE_ALLOWED: u8 = 0;
/// `ACCESS_ALLOWED_CALLBACK_ACE_TYPE`: same layout as the plain allow ACE up to the SID.
const ACE_TYPE_ALLOWED_CALLBACK: u8 = 9;
/// ACE types that can never grant access (deny, audit, alarm, mandatory label, resource
/// attribute and scoped policy ACEs). Every other type this module does not parse (compound
/// 4, object 5, callback-object 11 and anything unknown) fails closed.
const ACE_TYPES_NEVER_GRANT: [u8; 15] = [1, 2, 3, 6, 7, 8, 10, 12, 13, 14, 15, 16, 17, 18, 19];
/// Rights that let a principal change the directory or anything under it:
/// `FILE_WRITE_DATA`/`ADD_FILE`, `FILE_APPEND_DATA`/`ADD_SUBDIRECTORY`, `FILE_WRITE_EA`,
/// `FILE_DELETE_CHILD`, `FILE_WRITE_ATTRIBUTES`, `DELETE`, `WRITE_DAC`, `WRITE_OWNER`,
/// `GENERIC_WRITE` and `GENERIC_ALL`.
const WRITE_MASK: u32 = 0x0000_0002
    | 0x0000_0004
    | 0x0000_0010
    | 0x0000_0040
    | 0x0000_0100
    | 0x0001_0000
    | 0x0004_0000
    | 0x0008_0000
    | 0x4000_0000
    | 0x1000_0000;
/// SDDL of a directory only the current user, SYSTEM and Administrators can use. The
/// placeholder is replaced by a validated SID string.
const PRIVATE_DIR_SDDL: &str = "D:P(A;OICI;FA;;;{user})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";
const SID_SYSTEM: &str = "S-1-5-18";
const SID_ADMINISTRATORS: &str = "S-1-5-32-544";

/// Why a data directory failed the check.
#[derive(Debug, thiserror::Error)]
pub enum DirAclError {
    /// The directory is owned by someone other than the current user.
    #[error("data directory is not owned by the current user")]
    NotOwnedByCurrentUser,
    /// An allow ACE grants write access to another principal.
    #[error("data directory grants write access to {sid}")]
    WritableByOthers {
        /// The offending principal (a SID string, or `unknown` for an unparsed ACE).
        sid: String,
    },
    /// The directory has no DACL, which grants everyone full access.
    #[error("data directory has no access control list")]
    NullDacl,
    /// The owner or DACL could not be read.
    #[error(transparent)]
    Platform(#[from] PlatformError),
}

/// Verifies that `dir` is owned by the current user and writable only by the current user,
/// SYSTEM and Administrators.
///
/// # Errors
/// [`DirAclError`] when the directory fails the check or cannot be inspected.
pub fn verify_private_dir(dir: &Path) -> Result<(), DirAclError> {
    let current = Sid::current_user()?;
    let file = OpenOptions::new()
        .access_mode(READ_CONTROL)
        .share_mode(FILE_SHARE_ALL)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(dir)
        .map_err(PlatformError::from)?;
    let mut owner = PSID::default();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut psd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: `file` is a live directory handle opened with READ_CONTROL; every out pointer
    // is valid for the call.
    let status = unsafe {
        GetSecurityInfo(
            HANDLE(file.as_raw_handle()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            Some(&raw mut owner),
            None,
            Some(&raw mut dacl),
            None,
            Some(&raw mut psd),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(PlatformError::win32("GetSecurityInfo", status.0).into());
    }
    // SAFETY: GetSecurityInfo returned a LocalAlloc'd descriptor that owns `owner` and `dacl`;
    // the guard frees it after the last use below.
    let _psd_guard = unsafe { Owned::new(HLOCAL(psd.0)) };
    // SAFETY: `owner` points into the descriptor kept alive by `_psd_guard`.
    let owner_sid = unsafe { Sid::copy_from(owner) }?;
    if owner_sid != current {
        return Err(DirAclError::NotOwnedByCurrentUser);
    }
    // SAFETY: `dacl` is null or points into the descriptor kept alive by `_psd_guard`.
    unsafe { check_dacl(dacl, &current) }
}

/// Walks the ACEs of `dacl` and rejects write grants to anyone outside the trusted set.
///
/// # Safety
/// `dacl` must be null or point to a valid ACL that outlives the call.
unsafe fn check_dacl(dacl: *mut ACL, current: &Sid) -> Result<(), DirAclError> {
    if dacl.is_null() {
        return Err(DirAclError::NullDacl);
    }
    let trusted = [
        current.clone(),
        Sid::parse(SID_SYSTEM)?,
        Sid::parse(SID_ADMINISTRATORS)?,
    ];
    let mut info = ACL_SIZE_INFORMATION::default();
    // SAFETY: `dacl` is valid per the caller; `info` is a writable ACL_SIZE_INFORMATION.
    unsafe {
        GetAclInformation(
            dacl,
            (&raw mut info).cast(),
            u32::try_from(size_of::<ACL_SIZE_INFORMATION>()).unwrap_or(u32::MAX),
            AclSizeInformation,
        )
    }
    .map_err(win32_err("GetAclInformation"))?;
    for index in 0..info.AceCount {
        let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
        // SAFETY: `index` is below the ACE count reported for this ACL.
        unsafe { GetAce(dacl, index, &raw mut ace) }.map_err(win32_err("GetAce"))?;
        // SAFETY: GetAce returned a pointer to an ACE inside `dacl`.
        unsafe { check_ace(ace.cast::<u8>(), &trusted) }?;
    }
    Ok(())
}

/// Checks one ACE for a write grant to an untrusted principal.
///
/// # Safety
/// `ace` must point to a valid ACE (header, mask, SID) inside a live ACL.
unsafe fn check_ace(ace: *const u8, trusted: &[Sid; 3]) -> Result<(), DirAclError> {
    // SAFETY: every ACE starts with a one-byte type.
    let ace_type = unsafe { *ace };
    if ACE_TYPES_NEVER_GRANT.contains(&ace_type) {
        return Ok(()); // deny, audit and label ACEs never grant access
    }
    if ace_type != ACE_TYPE_ALLOWED && ace_type != ACE_TYPE_ALLOWED_CALLBACK {
        // An allow-family ACE this module cannot parse, or an unknown type: fail closed.
        return Err(DirAclError::WritableByOthers {
            sid: "unknown".to_owned(),
        });
    }
    // SAFETY: an allow ACE is {type, flags, size: u16, mask: u32, sid...}; the mask is at
    // byte 4 and the SID starts at byte 8. The ACE is only 4-byte aligned, so read unaligned.
    let mask = unsafe { ace.add(4).cast::<u32>().read_unaligned() };
    if mask & WRITE_MASK == 0 {
        return Ok(());
    }
    // SAFETY: the SID follows the mask inside the same ACE.
    let sid_ptr = PSID(unsafe { ace.add(8) }.cast_mut().cast());
    // SAFETY: `sid_ptr` points at the ACE's SID, which is readable for the call.
    let sid = unsafe { Sid::copy_from(sid_ptr) }?;
    if trusted.contains(&sid) {
        return Ok(());
    }
    Err(DirAclError::WritableByOthers {
        sid: sid.to_sid_string().unwrap_or_else(|_| "unknown".to_owned()),
    })
}

/// Creates `dir` (its parent must exist) with an explicit protected DACL: full control for
/// the current user, SYSTEM and Administrators, nothing inherited from the parent.
///
/// Returns `true` when the directory was created and `false` when it already existed (an
/// existing directory is left untouched; run [`verify_private_dir`] on it).
///
/// # Errors
/// [`PlatformError`] if the descriptor cannot be built or the directory cannot be created.
pub fn create_private_dir(dir: &Path) -> Result<bool, PlatformError> {
    let user = Sid::current_user()?.to_sid_string()?;
    let sddl = to_wide_nul(&PRIVATE_DIR_SDDL.replace("{user}", &user))?;
    let mut psd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: `sddl` is NUL-terminated and outlives the call; `psd` is a valid out pointer.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &raw mut psd,
            None,
        )
    }
    .map_err(win32_err(
        "ConvertStringSecurityDescriptorToSecurityDescriptorW",
    ))?;
    // SAFETY: the descriptor was allocated with LocalAlloc by the call above.
    let _guard = unsafe { Owned::new(HLOCAL(psd.0)) };
    let attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(0),
        lpSecurityDescriptor: psd.0,
        bInheritHandle: false.into(),
    };
    let absolute = std::path::absolute(dir)?;
    let wide = verbatim_wide(&absolute)?;
    // SAFETY: `wide` is NUL-terminated; `attributes` and the descriptor outlive the call.
    match unsafe { CreateDirectoryW(PCWSTR(wide.as_ptr()), Some(&raw const attributes)) } {
        Ok(()) => Ok(true),
        Err(e) if crate::error::unwrap_hresult(e.code().0) == ERROR_ALREADY_EXISTS.0 => Ok(false),
        Err(e) => Err(PlatformError::from_windows("CreateDirectoryW", &e)),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Everyone (`S-1-1-0`) as raw SID bytes.
    const EVERYONE: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0];

    fn ace(ace_type: u8, mask: u32) -> Vec<u8> {
        let mut bytes = vec![ace_type, 0, 20, 0];
        bytes.extend_from_slice(&mask.to_le_bytes());
        bytes.extend_from_slice(&EVERYONE);
        bytes
    }

    fn trusted() -> [Sid; 3] {
        [
            Sid::current_user().unwrap(),
            Sid::parse(SID_SYSTEM).unwrap(),
            Sid::parse(SID_ADMINISTRATORS).unwrap(),
        ]
    }

    fn check(ace_type: u8, mask: u32) -> Result<(), DirAclError> {
        let bytes = ace(ace_type, mask);
        // SAFETY: `bytes` is a well-formed ACE (header, mask, SID) that lives for the call.
        unsafe { check_ace(bytes.as_ptr(), &trusted()) }
    }

    #[test]
    fn a_callback_object_allow_ace_fails_closed() {
        assert!(matches!(
            check(11, 0x1F_01FF),
            Err(DirAclError::WritableByOthers { .. })
        ));
    }

    #[test]
    fn object_compound_and_unknown_allow_types_fail_closed() {
        for ace_type in [4_u8, 5, 11, 20, 200] {
            assert!(
                matches!(
                    check(ace_type, 1),
                    Err(DirAclError::WritableByOthers { .. })
                ),
                "type {ace_type}"
            );
        }
    }

    #[test]
    fn deny_audit_and_label_aces_are_ignored() {
        for ace_type in [1_u8, 2, 3, 6, 7, 8, 10, 12, 13, 14, 15, 16, 17] {
            assert!(check(ace_type, 0x1F_01FF).is_ok(), "type {ace_type}");
        }
    }

    #[test]
    fn generic_write_and_generic_all_grants_to_others_are_refused() {
        for mask in [0x4000_0000_u32, 0x1000_0000] {
            assert!(
                matches!(check(0, mask), Err(DirAclError::WritableByOthers { ref sid }) if sid == "S-1-1-0"),
                "mask {mask:#x}"
            );
        }
    }

    #[test]
    fn a_read_only_allow_ace_is_accepted() {
        assert!(check(0, 0x0012_0089).is_ok());
    }

    #[test]
    fn a_null_dacl_fails_closed() {
        let current = Sid::current_user().unwrap();
        // SAFETY: null is explicitly allowed by `check_dacl`.
        let result = unsafe { check_dacl(std::ptr::null_mut(), &current) };
        assert!(matches!(result, Err(DirAclError::NullDacl)));
    }
}
