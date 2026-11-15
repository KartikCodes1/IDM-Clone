//! Wide-string and verbatim path helpers.
//!
//! Pure Rust (no FFI): UTF-16 conversion that refuses interior NULs, and a builder for
//! `\\?\` verbatim paths used by the filesystem and MOTW slices.

use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf, Prefix};

use crate::error::PlatformError;

/// Converts `s` to UTF-16 with a trailing NUL. Rejects interior NULs.
///
/// # Errors
/// [`PlatformError::InvalidInput`] when `s` contains a NUL.
pub fn to_wide_nul(s: &str) -> Result<Vec<u16>, PlatformError> {
    os_to_wide_nul(OsStr::new(s))
}

/// Converts an `OsStr` to UTF-16 with a trailing NUL. Rejects interior NULs.
///
/// # Errors
/// [`PlatformError::InvalidInput`] when `s` contains a NUL.
pub fn os_to_wide_nul(s: &OsStr) -> Result<Vec<u16>, PlatformError> {
    let mut wide: Vec<u16> = s.encode_wide().collect();
    if wide.contains(&0) {
        return Err(PlatformError::InvalidInput(
            "string contains an interior NUL",
        ));
    }
    wide.push(0);
    Ok(wide)
}

/// Builds the `\\?\` verbatim form of an absolute path.
///
/// Verbatim paths bypass Win32 normalisation, so this refuses anything ambiguous: relative
/// or drive-relative paths, rooted paths without a drive, and `..` components. `.` and
/// repeated separators are dropped and `/` becomes `\`. An already-verbatim path is
/// returned unchanged. Device paths (`\\.\`) are rejected.
///
/// Component names are copied losslessly (unpaired UTF-16 surrogates are preserved) but are
/// NOT validated: callers must pass sanitiser output (THREAT_MODEL FS-01), and stream
/// suffixes such as `:Zone.Identifier` are intentionally allowed for the MOTW writer.
///
/// # Errors
/// [`PlatformError::InvalidInput`] for any of the cases above or an interior NUL.
pub fn verbatim_path(path: &Path) -> Result<PathBuf, PlatformError> {
    let mut parts = path.components();
    let first = parts
        .next()
        .ok_or(PlatformError::InvalidInput("empty path"))?;
    let Component::Prefix(prefix) = first else {
        return Err(PlatformError::InvalidInput("path is not absolute"));
    };
    let mut out = match prefix.kind() {
        Prefix::Verbatim(_) | Prefix::VerbatimDisk(_) | Prefix::VerbatimUNC(..) => {
            return reject_nul(path).map(|()| path.to_path_buf());
        }
        Prefix::Disk(letter) => {
            OsString::from(format!(r"\\?\{}:", char::from(letter.to_ascii_uppercase())))
        }
        Prefix::UNC(server, share) => {
            let mut unc = OsString::from(r"\\?\UNC\");
            unc.push(server);
            unc.push(r"\");
            unc.push(share);
            unc
        }
        Prefix::DeviceNS(_) => return Err(PlatformError::InvalidInput("device path")),
    };
    if !matches!(parts.next(), Some(Component::RootDir)) {
        return Err(PlatformError::InvalidInput("path is not absolute"));
    }
    for part in parts {
        match part {
            Component::Normal(name) => {
                out.push(r"\");
                out.push(name);
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(PlatformError::InvalidInput(
                    "path contains a '..' component",
                ));
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err(PlatformError::InvalidInput("malformed path"));
            }
        }
    }
    if out.encode_wide().last() == Some(u16::from(b':')) {
        out.push(r"\");
    }
    let built = PathBuf::from(out);
    reject_nul(&built)?;
    Ok(built)
}

/// [`verbatim_path`] followed by UTF-16 conversion with a trailing NUL.
///
/// # Errors
/// Same as [`verbatim_path`].
pub fn verbatim_wide(path: &Path) -> Result<Vec<u16>, PlatformError> {
    os_to_wide_nul(verbatim_path(path)?.as_os_str())
}

fn reject_nul(path: &Path) -> Result<(), PlatformError> {
    if path.as_os_str().encode_wide().any(|u| u == 0) {
        return Err(PlatformError::InvalidInput("path contains an interior NUL"));
    }
    Ok(())
}
