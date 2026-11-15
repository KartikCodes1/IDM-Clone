//! `show_in_folder`: reveal a completed download in Explorer.
//!
//! The path is never supplied by the webview. The agent reports the download's
//! `final_path`; this module checks that the download is completed and the file still
//! exists, then runs `%SystemRoot%\explorer.exe /select, <canonical path>` directly (no
//! shell, no other arguments). This is the only process spawn in the desktop crate.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use forgedm_protocol::{DownloadState, DownloadSummary};

use crate::error::{CommandError, CommandErrorKind};

/// A fully built, not yet executed Explorer invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplorerLaunch {
    /// Absolute path of `explorer.exe`.
    pub program: PathBuf,
    /// Exactly `/select,` followed by the canonical file path.
    pub args: Vec<OsString>,
}

fn refuse(message: &str) -> CommandError {
    CommandError::new(CommandErrorKind::InvalidRequest, message)
}

/// `explorer.exe` under the Windows directory.
#[must_use]
pub fn explorer_program(system_root: &Path) -> PathBuf {
    system_root.join("explorer.exe")
}

/// Removes a `\\?\C:\` verbatim prefix (Explorer does not understand it). UNC verbatim
/// paths are returned unchanged.
#[must_use]
pub fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") && rest.as_bytes().get(1) == Some(&b':') => {
            PathBuf::from(rest)
        }
        _ => path,
    }
}

/// Plans the launch for a completed download.
///
/// # Errors
/// [`CommandErrorKind::InvalidRequest`] when the download is not completed, has no final
/// path, or the file is gone or is not a regular file.
pub fn plan_launch(
    summary: &DownloadSummary,
    system_root: &Path,
) -> Result<ExplorerLaunch, CommandError> {
    if summary.state != DownloadState::Completed {
        return Err(refuse("Only completed downloads can be shown in a folder."));
    }
    let Some(final_path) = summary.final_path.as_deref() else {
        return Err(refuse("This download has no file to show."));
    };
    let gone = || refuse("The file no longer exists at its saved location.");
    let canonical = std::fs::canonicalize(final_path).map_err(|_| gone())?;
    if !canonical.is_file() {
        return Err(gone());
    }
    let canonical = strip_verbatim_prefix(canonical);
    Ok(ExplorerLaunch {
        program: explorer_program(system_root),
        args: vec![OsString::from("/select,"), canonical.into_os_string()],
    })
}

/// The Windows directory, from the environment.
///
/// # Errors
/// [`CommandErrorKind::Internal`] when neither `SystemRoot` nor `windir` is an absolute path.
pub fn system_root() -> Result<PathBuf, CommandError> {
    std::env::var_os("SystemRoot")
        .or_else(|| std::env::var_os("windir"))
        .map(PathBuf::from)
        .filter(|root| root.is_absolute())
        .ok_or_else(|| {
            CommandError::new(CommandErrorKind::Internal, "The Windows folder is unknown.")
        })
}

/// Starts Explorer for `plan` and does not wait for it.
///
/// # Errors
/// [`CommandErrorKind::Internal`] when the process cannot be started.
pub fn launch(plan: &ExplorerLaunch) -> Result<(), CommandError> {
    Command::new(&plan.program)
        .args(&plan.args)
        .spawn()
        .map(drop)
        .map_err(|err| {
            tracing::warn!(error = %err, "explorer could not be started");
            CommandError::new(CommandErrorKind::Internal, "Explorer could not be opened.")
        })
}
