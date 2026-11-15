//! Filesystem primitives: sparse files, file id, no-replace move, volume info, path safety.
//!
//! Every Win32 call here goes through `\\?\` verbatim paths from [`crate::wide`] (std's
//! long-path handling does not apply to direct calls). Files are never replaced: the only
//! move primitive uses `MoveFileExW` with `MOVEFILE_WRITE_THROUGH` and never
//! `MOVEFILE_REPLACE_EXISTING` (ARCHITECTURE 5.5, THREAT_MODEL FS-02). No handle is owned by
//! this module: every FFI call borrows a [`File`] or a local buffer, so cleanup is RAII.
#![allow(unsafe_code)] // Win32 file FFI; every unsafe block carries a SAFETY comment.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::time::Duration;

use windows::Win32::Foundation::{GetLastError, HANDLE, SetLastError, WIN32_ERROR};
use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_TAG_INFO, FILE_DISPOSITION_INFO, FILE_ID_INFO, FILE_NAME_NORMALIZED,
    FileAttributeTagInfo, FileDispositionInfo, FileIdInfo, GetCompressedFileSizeW,
    GetDiskFreeSpaceExW, GetFileInformationByHandleEx, GetFinalPathNameByHandleW,
    GetVolumeInformationByHandleW, MOVEFILE_WRITE_THROUGH, MoveFileExW, SetFileInformationByHandle,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{FILE_SET_SPARSE_BUFFER, FSCTL_SET_SPARSE};
use windows::core::PCWSTR;

use crate::error::{PlatformError, unwrap_hresult, win32_err};
use crate::wide::{verbatim_path, verbatim_wide};

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const DELETE: u32 = 0x0001_0000;
const FILE_SHARE_READ: u32 = 0x1;
const FILE_SHARE_ALL: u32 = 0x7;
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const FILE_SUPPORTS_SPARSE_FILES: u32 = 0x40;
const FILE_NAMED_STREAMS: u32 = 0x0004_0000;
/// `IsReparseTagNameSurrogate`: bit 29 of the reparse tag.
const REPARSE_TAG_NAME_SURROGATE_BIT: u32 = 0x2000_0000;
const INVALID_FILE_SIZE: u32 = u32::MAX;
const FS_NAME_CAPACITY: usize = 261;

const ERROR_ACCESS_DENIED: u32 = 5;
const ERROR_SHARING_VIOLATION: u32 = 32;
const ERROR_FILE_EXISTS: u32 = 80;
const ERROR_ALREADY_EXISTS: u32 = 183;
const ERROR_DISK_FULL: u32 = 112;
const ERROR_HANDLE_DISK_FULL: u32 = 39;
const ERROR_FILENAME_EXCED_RANGE: u32 = 206;
/// Codes meaning the volume or share went away (not ready, device missing, net gone, ...).
const DEVICE_GONE_CODES: [u32; 7] = [21, 53, 55, 67, 1005, 1006, 1167];

/// Highest `name (N).ext` suffix tried by [`finalize_move`].
pub const MAX_COLLISION_SUFFIX: u32 = 999;
const RETRY_INITIAL_DELAY: Duration = Duration::from_millis(100);
const RETRY_MAX_DELAY: Duration = Duration::from_secs(2);

/// Coarse classification of an I/O failure for the pause/fail policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoClass {
    /// The volume is full (Win32 112 or 39).
    DiskFull,
    /// The volume or share disappeared (removed, offline, not ready).
    DeviceGone,
    /// Access denied (Win32 5).
    AccessDenied,
    /// Sharing violation (Win32 32).
    Sharing,
    /// Anything else, including errors without an OS code.
    Other,
}

/// Classifies a raw Win32 error code.
#[must_use]
pub fn classify_code(code: u32) -> IoClass {
    match code {
        ERROR_DISK_FULL | ERROR_HANDLE_DISK_FULL => IoClass::DiskFull,
        ERROR_ACCESS_DENIED => IoClass::AccessDenied,
        ERROR_SHARING_VIOLATION => IoClass::Sharing,
        c if DEVICE_GONE_CODES.contains(&c) => IoClass::DeviceGone,
        _ => IoClass::Other,
    }
}

/// Classifies an [`io::Error`] by its raw OS code (never by message text).
#[must_use]
pub fn classify_io(err: &io::Error) -> IoClass {
    err.raw_os_error()
        .and_then(|c| u32::try_from(c).ok())
        .map_or(IoClass::Other, classify_code)
}

/// Maps a Win32 code to the most specific [`PlatformError`].
fn map_code(api: &'static str, code: u32) -> PlatformError {
    match code {
        ERROR_ALREADY_EXISTS | ERROR_FILE_EXISTS => PlatformError::AlreadyExists,
        ERROR_FILENAME_EXCED_RANGE => PlatformError::PathTooLong,
        _ => match classify_code(code) {
            IoClass::DiskFull => PlatformError::DiskFull,
            IoClass::Sharing => PlatformError::SharingViolation,
            IoClass::DeviceGone => PlatformError::DestinationUnavailable,
            IoClass::AccessDenied | IoClass::Other => PlatformError::win32(api, code),
        },
    }
}

fn map_io(api: &'static str, err: io::Error) -> PlatformError {
    match err.raw_os_error().and_then(|c| u32::try_from(c).ok()) {
        Some(code) => map_code(api, code),
        None => PlatformError::Io(err),
    }
}

fn raw_handle(file: &File) -> HANDLE {
    HANDLE(file.as_raw_handle())
}

fn size_u32<T>() -> u32 {
    u32::try_from(size_of::<T>()).unwrap_or(u32::MAX)
}

/// Turns the sparse attribute on or off (`FSCTL_SET_SPARSE`; works without admin on NTFS).
///
/// # Errors
/// [`PlatformError::Win32`] when the ioctl fails (for example the volume has no sparse support).
pub fn set_sparse(file: &File, sparse: bool) -> Result<(), PlatformError> {
    let buffer = FILE_SET_SPARSE_BUFFER { SetSparse: sparse };
    let mut returned = 0u32;
    // SAFETY: the handle is valid for the borrow of `file`; `buffer` and `returned` are
    // live locals; the input size matches `buffer`; there is no output buffer and no
    // OVERLAPPED, so the call is synchronous.
    unsafe {
        DeviceIoControl(
            raw_handle(file),
            FSCTL_SET_SPARSE,
            Some((&raw const buffer).cast()),
            size_u32::<FILE_SET_SPARSE_BUFFER>(),
            None,
            0,
            Some(&raw mut returned),
            None,
        )
    }
    .map_err(win32_err("DeviceIoControl(FSCTL_SET_SPARSE)"))
}

/// Bytes actually allocated on disk for `path` (`GetCompressedFileSizeW`; sparse holes and
/// compression make this smaller than the logical length).
///
/// # Errors
/// [`PlatformError::Win32`] when the file cannot be queried.
///
/// Path based and follows links: for diagnostics and tests, not for decisions about
/// untrusted paths (THREAT_MODEL residual 7).
pub fn allocated_size(path: &Path) -> Result<u64, PlatformError> {
    let wide = verbatim_wide(path)?;
    let mut high = 0u32;
    // SAFETY: clearing the thread error code is always sound.
    unsafe { SetLastError(WIN32_ERROR(0)) };
    // SAFETY: `wide` is NUL-terminated and outlives the call; `high` is a live local.
    let low = unsafe { GetCompressedFileSizeW(PCWSTR(wide.as_ptr()), Some(&raw mut high)) };
    if low == INVALID_FILE_SIZE {
        // SAFETY: reads the calling thread's last error; no pointers involved.
        let code = unsafe { GetLastError() }.0;
        if code != 0 {
            return Err(map_code("GetCompressedFileSizeW", code));
        }
    }
    Ok((u64::from(high) << 32) | u64::from(low))
}

/// Volume serial plus 128-bit file id (`FileIdInfo`). Stable across a same-volume rename on
/// NTFS, which reconcile relies on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileId {
    /// Volume serial number.
    pub volume_serial: u64,
    /// 128-bit file identifier.
    pub id: [u8; 16],
}

/// Reads the [`FileId`] of an open file.
///
/// # Errors
/// [`PlatformError::Win32`] when the file system does not support `FileIdInfo`.
pub fn file_id(file: &File) -> Result<FileId, PlatformError> {
    let mut info = FILE_ID_INFO::default();
    // SAFETY: valid handle; `info` is a live, correctly sized FILE_ID_INFO.
    unsafe {
        GetFileInformationByHandleEx(
            raw_handle(file),
            FileIdInfo,
            (&raw mut info).cast(),
            size_u32::<FILE_ID_INFO>(),
        )
    }
    .map_err(win32_err("GetFileInformationByHandleEx(FileIdInfo)"))?;
    Ok(FileId {
        volume_serial: info.VolumeSerialNumber,
        id: info.FileId.Identifier,
    })
}

/// File system facts that decide the preallocation strategy (ARCHITECTURE 5.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeInfo {
    /// File system name, for example `NTFS`, `exFAT`, `FAT32`.
    pub fs_name: String,
    /// The volume supports sparse files.
    pub supports_sparse: bool,
    /// The volume supports named streams (needed for `Zone.Identifier`).
    pub supports_streams: bool,
}

/// Queries the volume that holds `file`.
///
/// # Errors
/// [`PlatformError::Win32`] when the query fails.
pub fn volume_info(file: &File) -> Result<VolumeInfo, PlatformError> {
    let mut flags = 0u32;
    let mut name = [0u16; FS_NAME_CAPACITY];
    // SAFETY: valid handle; `flags` is a live local; `name` is a live buffer whose length
    // is passed implicitly by the slice; the volume-name buffer and other outputs are None.
    unsafe {
        GetVolumeInformationByHandleW(
            raw_handle(file),
            None,
            None,
            None,
            Some(&raw mut flags),
            Some(&mut name),
        )
    }
    .map_err(win32_err("GetVolumeInformationByHandleW"))?;
    let len = name.iter().position(|&u| u == 0).unwrap_or(name.len());
    Ok(VolumeInfo {
        fs_name: String::from_utf16_lossy(&name[..len]),
        supports_sparse: flags & FILE_SUPPORTS_SPARSE_FILES != 0,
        supports_streams: flags & FILE_NAMED_STREAMS != 0,
    })
}

/// Free bytes available to the calling user on the volume holding `dir`
/// (`GetDiskFreeSpaceExW`, quota aware).
///
/// # Errors
/// [`PlatformError::Win32`] when the directory cannot be queried.
pub fn free_space(dir: &Path) -> Result<u64, PlatformError> {
    let wide = verbatim_wide(dir)?;
    let mut free = 0u64;
    // SAFETY: `wide` is NUL-terminated and outlives the call; `free` is a live local.
    unsafe { GetDiskFreeSpaceExW(PCWSTR(wide.as_ptr()), Some(&raw mut free), None, None) }
        .map_err(win32_err("GetDiskFreeSpaceExW"))?;
    Ok(free)
}

/// Moves `src` to `dst` without ever replacing an existing file.
///
/// Uses `MoveFileExW` with `MOVEFILE_WRITE_THROUGH` only (no replace, no copy fallback, so
/// the move is a same-volume rename). `src` must not be open without `FILE_SHARE_DELETE`.
///
/// # Errors
/// [`PlatformError::AlreadyExists`] when `dst` exists (Win32 183/80);
/// [`PlatformError::SharingViolation`] for 32; other codes map per [`classify_code`].
pub fn move_no_replace(src: &Path, dst: &Path) -> Result<(), PlatformError> {
    let (s, d) = (verbatim_wide(src)?, verbatim_wide(dst)?);
    move_wide(&s, &d).map_err(|code| map_code("MoveFileExW", code))
}

fn move_wide(src: &[u16], dst: &[u16]) -> Result<(), u32> {
    // SAFETY: both buffers are NUL-terminated (built by `verbatim_wide`) and outlive the
    // call. MOVEFILE_REPLACE_EXISTING is deliberately absent.
    unsafe {
        MoveFileExW(
            PCWSTR(src.as_ptr()),
            PCWSTR(dst.as_ptr()),
            MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(|e| unwrap_hresult(e.code().0))
}

/// Candidate file name: `name`, then `stem (n).ext` (extension split at the last dot that is
/// not the first character).
fn candidate_name(name: &str, n: u32) -> String {
    if n == 0 {
        return name.to_owned();
    }
    match name.rfind('.') {
        Some(dot) if dot > 0 => format!("{} ({n}){}", &name[..dot], &name[dot..]),
        _ => format!("{name} ({n})"),
    }
}

/// Longest accepted final name in UTF-16 units (Appendix A.9 `NAME_MAX`); with the longest
/// collision suffix (` (999)`, 6 units) it still fits under the 255-unit component limit.
pub const NAME_MAX_UNITS: usize = 233;

/// Characters Win32 rejects in a file name (plus separators and `:`, which are never plain).
const FORBIDDEN_NAME_CHARS: [char; 9] = ['\\', '/', ':', '<', '>', '"', '|', '?', '*'];

fn is_dos_device_name(name: &str) -> bool {
    // Verbatim paths skip Win32 reserved-name handling, so fail closed here: the base name
    // (before the first dot, trailing spaces ignored) must not be a DOS device.
    let base = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    let upper = base.to_ascii_uppercase();
    if matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    upper
        .strip_prefix("COM")
        .or_else(|| upper.strip_prefix("LPT"))
        .is_some_and(|rest| rest.len() == 1 && rest.bytes().all(|b| b.is_ascii_digit()))
}

/// Defense in depth behind the S1.1 sanitiser: fails closed on anything that is not a plain,
/// creatable, round-trippable file name.
fn is_plain_file_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.ends_with(['.', ' '])
        && !name.contains(FORBIDDEN_NAME_CHARS)
        && !name.chars().any(char::is_control)
        && name.encode_utf16().count() <= NAME_MAX_UNITS
        && !is_dos_device_name(name)
}

/// Error for an exhausted retry budget: a persistent sharing violation stays
/// [`PlatformError::SharingViolation`]; a persistent access denial keeps its Win32 code.
fn exhausted_error(last_code: u32) -> PlatformError {
    map_code("MoveFileExW", last_code)
}

enum Attempt {
    Moved,
    Exists,
}

/// One candidate: retries 5/32 with doubling backoff drawn from `remaining`.
fn attempt_with_retry(
    src: &[u16],
    dst: &[u16],
    remaining: &mut Duration,
    sleeper: &mut dyn FnMut(Duration),
) -> Result<Attempt, PlatformError> {
    let mut delay = RETRY_INITIAL_DELAY;
    loop {
        match move_wide(src, dst) {
            Ok(()) => return Ok(Attempt::Moved),
            Err(ERROR_ALREADY_EXISTS | ERROR_FILE_EXISTS) => return Ok(Attempt::Exists),
            Err(code @ (ERROR_ACCESS_DENIED | ERROR_SHARING_VIOLATION)) => {
                if remaining.is_zero() {
                    return Err(exhausted_error(code));
                }
                let step = delay.min(*remaining);
                sleeper(step);
                *remaining -= step;
                delay = (delay * 2).min(RETRY_MAX_DELAY);
            }
            Err(code) => return Err(map_code("MoveFileExW", code)),
        }
    }
}

/// Finalizes `src` into `dir` as `name`, never clobbering an existing file.
///
/// Candidates are `name`, `stem (1).ext` ... `stem (999).ext`. `on_candidate` is called
/// with each candidate path BEFORE its first move attempt so the caller can persist it
/// (DUR-9); an error from it aborts without moving. Access denied (5) and sharing
/// violation (32) are retried with backoff (100 ms doubling, 2 s cap) until `budget` of
/// sleep time is used up; `sleeper` performs (or, in tests, records) each wait, so the
/// budget is deterministic and tests never sleep. The budget covers the whole call.
///
/// `name` must be S1.1 sanitiser output; this function re-checks it and fails closed (see
/// [`NAME_MAX_UNITS`]). A persisted candidate may be a file that already existed (the
/// callback fires before the move is tried), so recovery must compare file id or existence
/// before treating a persisted candidate as owned.
///
/// # Errors
/// [`PlatformError::InvalidInput`] for an unsafe name (empty, separators, `:`, Win32-invalid
/// or control characters, trailing dot or space, DOS device name, over [`NAME_MAX_UNITS`]);
/// [`PlatformError::SharingViolation`] when the budget runs out on 32, or
/// [`PlatformError::Win32`] code 5 when it runs out on access denied;
/// [`PlatformError::AlreadyExists`] when all 1000 candidates exist; other move errors as
/// per [`move_no_replace`]; whatever `on_candidate` returns.
pub fn finalize_move(
    src: &Path,
    dir: &Path,
    name: &str,
    on_candidate: &mut dyn FnMut(&Path) -> Result<(), PlatformError>,
    budget: Duration,
    sleeper: &mut dyn FnMut(Duration),
) -> Result<PathBuf, PlatformError> {
    if !is_plain_file_name(name) {
        return Err(PlatformError::InvalidInput("invalid final file name"));
    }
    let src_wide = verbatim_wide(src)?;
    let mut remaining = budget;
    for n in 0..=MAX_COLLISION_SUFFIX {
        let candidate = dir.join(candidate_name(name, n));
        let candidate_wide = verbatim_wide(&candidate)?;
        on_candidate(&candidate)?;
        match attempt_with_retry(&src_wide, &candidate_wide, &mut remaining, sleeper)? {
            Attempt::Moved => return Ok(candidate),
            Attempt::Exists => {}
        }
    }
    Err(PlatformError::AlreadyExists)
}

/// Creates the staging file: read+write+`create_new`, `FILE_SHARE_READ` for other
/// processes, `FILE_FLAG_OPEN_REPARSE_POINT` (a planted link fails as `AlreadyExists`),
/// plus DELETE access so the file can be removed by handle ([`delete_by_handle`]).
///
/// # Errors
/// [`PlatformError::AlreadyExists`] when the name exists (including a planted link);
/// other failures map per [`classify_code`].
pub fn create_part_file(path: &Path) -> Result<File, PlatformError> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .access_mode(GENERIC_READ | GENERIC_WRITE | DELETE)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(verbatim_path(path)?)
        .map_err(|e| map_io("CreateFileW", e))
}

/// Reopens an existing staging file for resume (no create), with the same share mode, not
/// following a reparse point.
///
/// # Errors
/// [`PlatformError::PathEscape`] when the path is a reparse point; open failures map per
/// [`classify_code`].
pub fn open_part_file(path: &Path) -> Result<File, PlatformError> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .access_mode(GENERIC_READ | GENERIC_WRITE | DELETE)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(verbatim_path(path)?)
        .map_err(|e| map_io("CreateFileW", e))?;
    if attribute_tag(&file)?.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(PlatformError::PathEscape);
    }
    Ok(file)
}

/// Marks an open file for deletion (closed handle deletes it). Needs a handle opened with
/// DELETE access, which [`create_part_file`] provides. Deleting by handle, never by name,
/// cannot hit a swapped-in file.
///
/// # Errors
/// [`PlatformError::Win32`] when the disposition cannot be set.
pub fn delete_by_handle(file: &File) -> Result<(), PlatformError> {
    let info = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: valid handle; `info` is a live, correctly sized FILE_DISPOSITION_INFO.
    unsafe {
        SetFileInformationByHandle(
            raw_handle(file),
            FileDispositionInfo,
            (&raw const info).cast(),
            size_u32::<FILE_DISPOSITION_INFO>(),
        )
    }
    .map_err(win32_err("SetFileInformationByHandle(Disposition)"))
}

/// The normalised final path of an open file (`GetFinalPathNameByHandleW`), after every
/// link has been resolved. Usually in `\\?\C:\...` form.
///
/// # Errors
/// [`PlatformError::Win32`] when the path cannot be resolved.
pub fn final_path(file: &File) -> Result<PathBuf, PlatformError> {
    let mut buf = vec![0u16; 512];
    loop {
        // SAFETY: valid handle; `buf` is a live writable buffer, its length is passed by
        // the slice. FILE_NAME_NORMALIZED (0) also selects the DOS volume name form.
        let n =
            unsafe { GetFinalPathNameByHandleW(raw_handle(file), &mut buf, FILE_NAME_NORMALIZED) };
        let n = usize::try_from(n).unwrap_or(usize::MAX);
        if n == 0 {
            // SAFETY: reads the calling thread's last error; no pointers involved.
            let code = unsafe { GetLastError() }.0;
            return Err(map_code("GetFinalPathNameByHandleW", code));
        }
        if n >= buf.len() {
            buf.resize(n + 1, 0); // needed size includes the NUL
            continue;
        }
        return Ok(PathBuf::from(OsString::from_wide(&buf[..n])));
    }
}

fn fold_unit(unit: u16) -> u16 {
    match char::from_u32(u32::from(unit)) {
        Some(c) => {
            let mut lower = c.to_lowercase();
            match (lower.next(), lower.next()) {
                (Some(l), None) => u16::try_from(u32::from(l)).unwrap_or(unit),
                _ => unit,
            }
        }
        None => unit,
    }
}

/// Normalised comparison form: verbatim prefix removed, `/` as `\`, no trailing `\`,
/// case folded per UTF-16 unit.
fn comparable(path: &Path) -> Vec<u16> {
    let mut units: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .map(|u| {
            if u == u16::from(b'/') {
                u16::from(b'\\')
            } else {
                u
            }
        })
        .collect();
    let unc: Vec<u16> = r"\\?\UNC\".encode_utf16().collect();
    let verbatim: Vec<u16> = r"\\?\".encode_utf16().collect();
    if units.starts_with(&unc) {
        units.splice(..unc.len(), r"\\".encode_utf16());
    } else if units.starts_with(&verbatim) {
        units.drain(..verbatim.len());
    }
    while units.last() == Some(&u16::from(b'\\')) {
        units.pop();
    }
    units.into_iter().map(fold_unit).collect()
}

/// Verifies that the open file lives strictly below `canonical_root` once every junction
/// and symlink is resolved (the FS-02 check made after creating the staging file).
///
/// The comparison is case-insensitive and respects path boundaries (`dl-evil` is not inside
/// `dl`).
///
/// # Errors
/// [`PlatformError::PathEscape`] when the file is elsewhere; resolution errors as per
/// [`final_path`].
pub fn ensure_within(file: &File, canonical_root: &Path) -> Result<(), PlatformError> {
    let actual = comparable(&final_path(file)?);
    let mut root = comparable(canonical_root);
    if root.is_empty() {
        return Err(PlatformError::InvalidInput("empty root"));
    }
    root.push(u16::from(b'\\'));
    if actual.len() > root.len() && actual.starts_with(&root) {
        Ok(())
    } else {
        Err(PlatformError::PathEscape)
    }
}

/// `IsReparseTagNameSurrogate`: symlinks and junctions are name surrogates; cloud
/// placeholders (OneDrive and friends) and WOF are not.
#[must_use]
pub const fn reparse_tag_is_name_surrogate(tag: u32) -> bool {
    tag & REPARSE_TAG_NAME_SURROGATE_BIT != 0
}

fn attribute_tag(file: &File) -> Result<FILE_ATTRIBUTE_TAG_INFO, PlatformError> {
    let mut info = FILE_ATTRIBUTE_TAG_INFO::default();
    // SAFETY: valid handle; `info` is a live, correctly sized FILE_ATTRIBUTE_TAG_INFO.
    unsafe {
        GetFileInformationByHandleEx(
            raw_handle(file),
            FileAttributeTagInfo,
            (&raw mut info).cast(),
            size_u32::<FILE_ATTRIBUTE_TAG_INFO>(),
        )
    }
    .map_err(win32_err("GetFileInformationByHandleEx(AttributeTag)"))?;
    Ok(info)
}

/// Whether the entry at `path` itself (links are not followed) is a name-surrogate reparse
/// point (symlink or junction). Plain files, directories and cloud placeholders give `false`.
///
/// # Errors
/// Open or query failures, for example when `path` does not exist.
pub fn is_name_surrogate(path: &Path) -> Result<bool, PlatformError> {
    let file = OpenOptions::new()
        .access_mode(0) // metadata only
        .share_mode(FILE_SHARE_ALL)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(verbatim_path(path)?)
        .map_err(|e| map_io("CreateFileW", e))?;
    let info = attribute_tag(&file)?;
    Ok(info.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        && reparse_tag_is_name_surrogate(info.ReparseTag))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhausted_access_denied_keeps_its_win32_code() {
        assert!(matches!(
            exhausted_error(ERROR_ACCESS_DENIED),
            PlatformError::Win32 { code: 5, .. }
        ));
        assert!(matches!(
            exhausted_error(ERROR_SHARING_VIOLATION),
            PlatformError::SharingViolation
        ));
    }
}
