//! The data root and the pipe name (PHASE_0_1_PLAN A.10, ARCHITECTURE 2.2 and 2.4).
//!
//! The agent and every client derive the pipe name through [`pipe_name_for`], so they cannot
//! disagree. The user SID comes from the process token and the default root from
//! `SHGetKnownFolderPath`, never from environment variables (spoofable).

use std::path::{Component, Path, PathBuf};

use forgedm_platform::PlatformError;
use forgedm_platform::known_folders;
use forgedm_platform::sid::Sid;
use sha2::{Digest, Sha256};

/// Folder under `%LOCALAPPDATA%` that holds the default data root.
const DEFAULT_ROOT_NAME: &str = "ForgeDM";
/// Pipe namespace prefix; the user SID follows.
const PIPE_PREFIX: &str = r"\\.\pipe\ForgeDM-";
/// Hex characters of the data-root hash appended to a non-default pipe name.
const ROOT_HASH_HEX_LEN: usize = 16;

/// `%LOCALAPPDATA%\ForgeDM` from `SHGetKnownFolderPath`.
///
/// # Errors
/// [`PlatformError`] when the shell cannot resolve the folder.
pub fn default_data_root() -> Result<PathBuf, PlatformError> {
    Ok(known_folders::local_app_data()?.join(DEFAULT_ROOT_NAME))
}

/// The canonical form of `path`, or for a directory that does not exist yet the form it will
/// have once created: `.` and `..` resolved lexically, then the deepest existing ancestor
/// canonicalised and the remaining components appended. The agent hashes the canonical form
/// after creating the directory, so a client hashing before creation must agree.
fn canonical_or_projected(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    let mut lexical = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // Never pops a prefix or root; `..` at the root stays at the root.
                if matches!(lexical.components().next_back(), Some(Component::Normal(_))) {
                    lexical.pop();
                }
            }
            other => lexical.push(other.as_os_str()),
        }
    }
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    let mut existing = lexical.as_path();
    loop {
        if let Ok(canonical) = std::fs::canonicalize(existing) {
            let mut result = canonical;
            result.extend(missing.iter().rev());
            return result;
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name.to_owned());
                existing = parent;
            }
            _ => return lexical,
        }
    }
}

/// Canonical text of a data root for comparison and hashing: canonicalised when it exists,
/// verbatim prefix removed, lowercased, no trailing separator.
#[must_use]
pub fn normalized_root(path: &Path) -> String {
    let canonical = canonical_or_projected(path);
    let lower = canonical.to_string_lossy().to_lowercase();
    let stripped = if let Some(unc) = lower.strip_prefix(r"\\?\unc\") {
        format!(r"\\{unc}")
    } else {
        lower
            .strip_prefix(r"\\?\")
            .map_or_else(|| lower.clone(), str::to_owned)
    };
    let trimmed = stripped.trim_end_matches('\\');
    // "c:" alone would mean the current directory of the drive; keep the root separator.
    if trimmed.ends_with(':') {
        format!("{trimmed}\\")
    } else {
        trimmed.to_owned()
    }
}

/// First 16 hex characters of SHA-256 over the UTF-16LE bytes of [`normalized_root`].
#[must_use]
pub fn root_hash(root: &Path) -> String {
    let bytes: Vec<u8> = normalized_root(root)
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let mut hex = hex::encode(Sha256::digest(bytes));
    hex.truncate(ROOT_HASH_HEX_LEN);
    hex
}

/// The pipe name of the agent serving `data_root` for the current user (A.10):
/// `\\.\pipe\ForgeDM-<user SID>`, plus `-<root hash>` when the root is not the default one.
/// `None`, or a root equal to the default root, gives the plain name.
///
/// # Errors
/// [`PlatformError`] when the user SID or the default root cannot be resolved.
pub fn pipe_name_for(data_root: Option<&Path>) -> Result<String, PlatformError> {
    let sid = Sid::current_user()?.to_sid_string()?;
    let non_default = match data_root {
        Some(root) if normalized_root(root) != normalized_root(&default_data_root()?) => Some(root),
        _ => None,
    };
    Ok(match non_default {
        Some(root) => format!("{PIPE_PREFIX}{sid}-{}", root_hash(root)),
        None => format!("{PIPE_PREFIX}{sid}"),
    })
}
