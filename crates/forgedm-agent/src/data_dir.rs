//! The data directory: creation and the reparse-point check (ARCHITECTURE 2.2, 2.3 step 2).
//!
//! Layout: `forgedm.db` (+ `-wal`, `-shm`), `logs\`, `run\agent.lock`. Before anything is
//! created or opened under the root, the root and each fixed child that already exists must
//! not be a name-surrogate reparse point (symbolic link or junction): the agent must never
//! write its database, lock or logs through a redirect.
//!
//! An explicit `--data-dir` gets a stricter check ([`DataPaths::prepare_explicit`]): it must
//! be owned by the current user and no ACE may grant write access to anyone but the
//! current user, SYSTEM or Administrators. A directory the agent creates itself is created
//! with an explicit protected DACL. The default `%LOCALAPPDATA%\ForgeDM` keeps the ACL it
//! inherits from the user profile (the profile is already restricted to those principals),
//! so it only gets the reparse-point check.

use std::io;
use std::path::{Path, PathBuf};

use forgedm_platform::acl::{DirAclError, create_private_dir, verify_private_dir};

const DB_FILE: &str = "forgedm.db";
const LOGS_DIR: &str = "logs";
const RUN_DIR: &str = "run";
const LOCK_FILE: &str = "agent.lock";
/// Fixed children that must never be redirected.
const GUARDED_CHILDREN: [&str; 5] = [
    LOGS_DIR,
    RUN_DIR,
    DB_FILE,
    "forgedm.db-wal",
    "forgedm.db-shm",
];

/// Why the data directory cannot be used. The agent exits with code 6.
#[derive(Debug, thiserror::Error)]
pub enum DataDirError {
    /// The root or a child could not be created or inspected.
    #[error("cannot prepare {path}: {source}")]
    Io {
        /// The path being prepared.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: io::Error,
    },
    /// The root or one of its fixed children is a symbolic link or junction.
    #[error("{path} is a reparse point; refusing to use it")]
    ReparsePoint {
        /// The redirected path.
        path: PathBuf,
    },
    /// An explicit data directory is not private to the current user.
    #[error("{path} is not private to the current user: {source}")]
    Acl {
        /// The directory that failed the check.
        path: PathBuf,
        /// What is wrong with it.
        #[source]
        source: DirAclError,
    },
}

/// Resolved locations under a validated data root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataPaths {
    /// The canonical data root (verbatim prefix removed).
    pub root: PathBuf,
    /// `forgedm.db`.
    pub db: PathBuf,
    /// `logs\`.
    pub logs: PathBuf,
    /// `run\agent.lock`.
    pub lock: PathBuf,
}

impl DataPaths {
    /// Creates the root, `logs` and `run`, after checking nothing is redirected.
    ///
    /// # Errors
    /// [`DataDirError::ReparsePoint`] for a redirected root or child, otherwise
    /// [`DataDirError::Io`].
    pub fn prepare(root: &Path) -> Result<Self, DataDirError> {
        std::fs::create_dir_all(root).map_err(|e| io_error(root, e))?;
        Self::finish(root)
    }

    /// [`Self::prepare`] for an explicit `--data-dir`: a missing root is created with a
    /// protected DACL (its parents are created normally), and the root must then pass the
    /// owner and DACL check.
    ///
    /// # Errors
    /// [`DataDirError::Acl`] when the directory is not private, otherwise as
    /// [`Self::prepare`].
    pub fn prepare_explicit(root: &Path) -> Result<Self, DataDirError> {
        if let Some(parent) = root.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io_error(parent, e))?;
        }
        reject_reparse_point(root)?;
        create_private_dir(root).map_err(|e| acl_error(root, e.into()))?;
        reject_reparse_point(root)?;
        verify_private_dir(root).map_err(|e| acl_error(root, e))?;
        Self::finish(root)
    }

    fn finish(root: &Path) -> Result<Self, DataDirError> {
        reject_reparse_point(root)?;
        for name in GUARDED_CHILDREN {
            reject_reparse_point(&root.join(name))?;
        }
        for name in [LOGS_DIR, RUN_DIR] {
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).map_err(|e| io_error(&dir, e))?;
        }
        let canonical = std::fs::canonicalize(root).map_err(|e| io_error(root, e))?;
        let root = strip_verbatim(canonical);
        Ok(Self {
            db: root.join(DB_FILE),
            logs: root.join(LOGS_DIR),
            lock: root.join(RUN_DIR).join(LOCK_FILE),
            root,
        })
    }
}

fn acl_error(path: &Path, source: DirAclError) -> DataDirError {
    DataDirError::Acl {
        path: path.to_path_buf(),
        source,
    }
}

fn io_error(path: &Path, source: io::Error) -> DataDirError {
    DataDirError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Fails when `path` exists and is a symbolic link or junction. A missing path is fine.
///
/// `FileType::is_symlink` is true exactly for name-surrogate reparse points on Windows.
fn reject_reparse_point(path: &Path) -> Result<(), DataDirError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(DataDirError::ReparsePoint {
            path: path.to_path_buf(),
        }),
        Ok(_) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(io_error(path, err)),
    }
}

/// `\\?\C:\x` becomes `C:\x`; `\\?\UNC\h\s` becomes `\\h\s`.
fn strip_verbatim(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{unc}"));
    }
    match text.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_creates_layout_and_returns_plain_canonical_paths() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("nested").join("root");

        let paths = DataPaths::prepare(&root).unwrap();

        assert!(paths.logs.is_dir());
        assert!(paths.lock.parent().unwrap().is_dir());
        assert!(!paths.root.to_string_lossy().starts_with(r"\\?\"));
        assert_eq!(paths.db.file_name().unwrap(), "forgedm.db");
        assert_eq!(paths.lock.file_name().unwrap(), "agent.lock");
    }

    #[test]
    fn prepare_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let first = DataPaths::prepare(dir.path()).unwrap();
        let second = DataPaths::prepare(dir.path()).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn a_file_where_the_root_should_be_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-dir");
        std::fs::write(&file, b"x").unwrap();
        let err = DataPaths::prepare(&file).unwrap_err();
        assert!(matches!(err, DataDirError::Io { .. }), "{err}");
    }

    #[test]
    fn a_symlinked_child_is_rejected_when_symlinks_are_available() {
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        if std::os::windows::fs::symlink_dir(other.path(), dir.path().join("run")).is_err() {
            return; // no symlink privilege on this machine; covered by the process test
        }
        let err = DataPaths::prepare(dir.path()).unwrap_err();
        assert!(matches!(err, DataDirError::ReparsePoint { .. }), "{err}");
        assert_eq!(std::fs::read_dir(other.path()).unwrap().count(), 0);
    }

    #[test]
    fn an_explicit_missing_root_is_created_private_and_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("data");

        let paths = DataPaths::prepare_explicit(&root).unwrap();

        assert!(paths.logs.is_dir() && paths.lock.parent().unwrap().is_dir());
        verify_private_dir(&paths.root).unwrap();
        // A second start over the directory it created passes the check as well.
        assert_eq!(DataPaths::prepare_explicit(&root).unwrap(), paths);
    }

    #[test]
    fn an_explicit_root_that_is_a_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-dir");
        std::fs::write(&file, b"x").unwrap();
        let err = DataPaths::prepare_explicit(&file).unwrap_err();
        assert!(!matches!(err, DataDirError::ReparsePoint { .. }), "{err}");
    }

    #[test]
    fn strip_verbatim_handles_drive_and_unc_forms() {
        assert_eq!(
            strip_verbatim(PathBuf::from(r"\\?\C:\a")),
            PathBuf::from(r"C:\a")
        );
        assert_eq!(
            strip_verbatim(PathBuf::from(r"\\?\UNC\h\s\a")),
            PathBuf::from(r"\\h\s\a")
        );
        assert_eq!(
            strip_verbatim(PathBuf::from(r"C:\a")),
            PathBuf::from(r"C:\a")
        );
    }
}
