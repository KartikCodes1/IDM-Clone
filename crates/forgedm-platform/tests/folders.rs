//! Known folders and the single-instance lock.

use forgedm_platform::instance_lock::{InstanceLock, LockError};
use forgedm_platform::known_folders;

#[test]
fn known_folders_downloads_and_localappdata_absolute_dirs() {
    for dir in [
        known_folders::downloads().expect("downloads"),
        known_folders::local_app_data().expect("localappdata"),
    ] {
        assert!(dir.is_absolute(), "{dir:?}");
        assert!(dir.is_dir(), "{dir:?}");
    }
}

#[test]
fn instance_lock_excludes_second_and_releases_on_drop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("run").join("agent.lock");

    let first = InstanceLock::try_acquire(&path).expect("first acquires");
    let second = InstanceLock::try_acquire(&path);
    assert!(
        matches!(second, Err(LockError::AlreadyHeld)),
        "got {second:?}"
    );

    drop(first);
    let third = InstanceLock::try_acquire(&path).expect("acquires after drop");
    assert_eq!(third.path(), path.as_path());
}

#[test]
fn instance_lock_unusable_path_is_io_error_not_already_held() {
    let dir = tempfile::tempdir().expect("tempdir");
    // The parent "directory" is a file, so the lock file cannot be created.
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"x").expect("write");
    let r = InstanceLock::try_acquire(&blocker.join("agent.lock"));
    assert!(matches!(r, Err(LockError::Io(_))), "got {r:?}");
}
