//! Single-instance lock file.
//!
//! An exclusive `File::try_lock` (LockFileEx) on `run\agent.lock`. The OS releases it when
//! the holder exits or crashes, so a killed agent never leaves a stale lock.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

/// Failure to take the instance lock.
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    /// Another holder (process or handle) owns the lock.
    #[error("another instance holds the lock")]
    AlreadyHeld,
    /// The lock file could not be created or locked for another reason.
    #[error("lock file error: {0}")]
    Io(#[from] io::Error),
}

/// An exclusive lock held for as long as the value lives.
#[derive(Debug)]
pub struct InstanceLock {
    // Held only to keep the OS lock; dropping closes the handle and releases it.
    _file: File,
    path: PathBuf,
}

impl InstanceLock {
    /// Creates the lock file (and its parent directory) and takes an exclusive lock.
    ///
    /// Does not block or retry; callers that want a grace period retry themselves.
    ///
    /// # Errors
    /// [`LockError::AlreadyHeld`] when the lock is taken, [`LockError::Io`] otherwise.
    pub fn try_acquire(path: &Path) -> Result<Self, LockError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Self {
                _file: file,
                path: path.to_path_buf(),
            }),
            Err(TryLockError::WouldBlock) => Err(LockError::AlreadyHeld),
            Err(TryLockError::Error(e)) => Err(LockError::Io(e)),
        }
    }

    /// The lock file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}
