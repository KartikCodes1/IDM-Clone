//! Validation of the values that decide where a file lands: the destination directory, the
//! requested file name and the expected checksum (ARCHITECTURE 5.7, PLAN A.7).
//!
//! Errors carry only a machine reason; they never echo the supplied path.

use std::path::{Path, PathBuf};

use forgedm_core::naming::sanitize_opt;
use forgedm_platform::fs::is_name_surrogate;
use forgedm_platform::known_folders;
use forgedm_protocol::{InvalidDestinationReason as Reason, SHA256_HEX_LEN};

use crate::error::AgentError;

/// Device namespaces: `\\.\` and the global root. Direct device access is never a download
/// destination.
fn is_device_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.starts_with(r"\\.\")
        || lower.starts_with(r"\\?\globalroot")
        || is_verbatim_non_drive(&lower)
}

/// `\\?\` followed by anything but `<drive>:\` or `UNC\`.
fn is_verbatim_non_drive(lower: &str) -> bool {
    let Some(rest) = lower.strip_prefix(r"\\?\") else {
        return false;
    };
    if rest.starts_with(r"unc\") {
        return false;
    }
    let mut chars = rest.chars();
    let drive = chars.next().is_some_and(|c| c.is_ascii_alphabetic());
    let colon = chars.next() == Some(':');
    let root = matches!(chars.next(), Some('\\') | None);
    !(drive && colon && root)
}

/// `\\server\share` and `\\?\UNC\...`.
fn is_unc_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.starts_with(r"\\?\unc\") || (lower.starts_with(r"\\") && !lower.starts_with(r"\\?\"))
}

/// Checks the text of a destination before touching the file system.
fn check_syntax(raw: &str) -> Result<(), Reason> {
    let normalised = raw.replace('/', r"\");
    if is_device_path(&normalised) {
        return Err(Reason::DevicePath);
    }
    if is_unc_path(&normalised) {
        return Err(Reason::UncUnsupported);
    }
    if !Path::new(raw).is_absolute() {
        return Err(Reason::NotAbsolute);
    }
    Ok(())
}

/// Checks that `path` exists, is a directory and is not a symlink or junction.
fn check_directory(path: &Path) -> Result<(), Reason> {
    match is_name_surrogate(path) {
        Ok(true) => return Err(Reason::ReparsePoint),
        Ok(false) => {}
        Err(_) => return Err(Reason::NotFound),
    }
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(Reason::NotDirectory),
        Err(_) => Err(Reason::NotFound),
    }
}

/// Validates `target_dir` (or the Downloads folder when absent) and returns its canonical
/// `\\?\` form, the form stored with the download.
///
/// # Errors
/// [`AgentError::InvalidDestination`] with the first rule broken.
pub fn validate_destination(target_dir: Option<&str>) -> Result<PathBuf, AgentError> {
    let path = match target_dir {
        Some(raw) => {
            check_syntax(raw).map_err(AgentError::InvalidDestination)?;
            PathBuf::from(raw)
        }
        None => known_folders::downloads().map_err(|err| {
            tracing::error!(error = %err, "the Downloads folder cannot be resolved");
            AgentError::InvalidDestination(Reason::NotFound)
        })?,
    };
    check_directory(&path).map_err(AgentError::InvalidDestination)?;
    let canonical = std::fs::canonicalize(&path)
        .map_err(|_| AgentError::InvalidDestination(Reason::NotFound))?;
    if canonical
        .to_string_lossy()
        .to_ascii_lowercase()
        .starts_with(r"\\?\unc\")
    {
        return Err(AgentError::InvalidDestination(Reason::UncUnsupported));
    }
    Ok(canonical)
}

/// The sanitised requested file name, `None` when the client asked for none.
///
/// # Errors
/// [`AgentError::InvalidFileName`] when nothing usable remains after sanitising.
pub fn validate_file_name(requested: Option<&str>) -> Result<Option<String>, AgentError> {
    requested
        .map(|raw| sanitize_opt(raw).ok_or(AgentError::InvalidFileName))
        .transpose()
}

/// The expected checksum in its normalised (lowercase) form.
///
/// # Errors
/// [`AgentError::InvalidChecksum`] unless it is exactly 64 hex characters.
pub fn validate_checksum(expected: Option<&str>) -> Result<Option<String>, AgentError> {
    expected
        .map(|raw| {
            let valid = raw.len() == SHA256_HEX_LEN && raw.bytes().all(|b| b.is_ascii_hexdigit());
            if valid {
                Ok(raw.to_ascii_lowercase())
            } else {
                Err(AgentError::InvalidChecksum)
            }
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reason(raw: &str) -> Option<Reason> {
        check_syntax(raw).err()
    }

    #[test]
    fn syntax_classifies_device_unc_and_relative_paths() {
        assert_eq!(reason(r"\\.\C:\x"), Some(Reason::DevicePath));
        assert_eq!(reason(r"\\?\GLOBALROOT\Device\X"), Some(Reason::DevicePath));
        assert_eq!(reason(r"\\?\Volume{1234}\x"), Some(Reason::DevicePath));
        assert_eq!(reason(r"\\server\share\x"), Some(Reason::UncUnsupported));
        assert_eq!(reason("//server/share/x"), Some(Reason::UncUnsupported));
        assert_eq!(
            reason(r"\\?\UNC\server\share"),
            Some(Reason::UncUnsupported)
        );
        assert_eq!(reason(r"relative\dir"), Some(Reason::NotAbsolute));
        assert_eq!(reason(r"\rooted"), Some(Reason::NotAbsolute));
        assert_eq!(reason(r"C:drive-relative"), Some(Reason::NotAbsolute));
        assert_eq!(reason(r"C:\Users\x"), None);
        assert_eq!(reason(r"\\?\C:\Users\x"), None);
    }

    #[test]
    fn a_real_directory_is_canonicalised() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = validate_destination(dir.path().to_str()).unwrap();
        assert!(canonical.is_absolute());
        assert!(canonical.to_string_lossy().starts_with(r"\\?\"));
    }

    #[test]
    fn file_names_are_sanitised_and_checksums_normalised() {
        assert_eq!(
            validate_file_name(Some(r"..\..\evil.exe"))
                .unwrap()
                .as_deref(),
            Some("evil.exe")
        );
        assert!(matches!(
            validate_file_name(Some("...")),
            Err(AgentError::InvalidFileName)
        ));
        assert_eq!(validate_file_name(None).unwrap(), None);
        let upper = "AB".repeat(32);
        assert_eq!(
            validate_checksum(Some(&upper)).unwrap().as_deref(),
            Some(upper.to_ascii_lowercase().as_str())
        );
        assert!(validate_checksum(Some("xyz")).is_err());
        assert!(validate_checksum(Some(&"g".repeat(64))).is_err());
    }
}
