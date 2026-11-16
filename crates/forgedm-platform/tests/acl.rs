//! Data-directory owner and DACL check (S1.14): `verify_private_dir`, `create_private_dir`.
#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers

use std::path::Path;
use std::process::Command;

use forgedm_platform::acl::{DirAclError, create_private_dir, verify_private_dir};

/// A fresh directory with a protected DACL. The temp directory itself is not used directly
/// because its inherited ACL depends on the machine (some hosts add sandbox groups to it).
fn fresh_private_dir() -> (tempfile::TempDir, std::path::PathBuf) {
    let parent = tempfile::tempdir().unwrap();
    let dir = parent.path().join("data");
    assert!(create_private_dir(&dir).unwrap());
    (parent, dir)
}

/// Grants `principal` the icacls `rights` (for example `F` or `R`) on `dir`, inherited.
fn grant(dir: &Path, principal: &str, rights: &str) {
    let out = Command::new("icacls")
        .arg(dir)
        .arg("/grant")
        .arg(format!("{principal}:(OI)(CI){rights}"))
        .output()
        .expect("icacls runs");
    assert!(
        out.status.success(),
        "icacls failed: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn a_fresh_user_dir_is_accepted() {
    let (_parent, dir) = fresh_private_dir();
    verify_private_dir(&dir).expect("a fresh directory is private");
}

#[test]
fn a_world_writable_dir_is_refused() {
    let (_parent, dir) = fresh_private_dir();
    grant(&dir, "*S-1-1-0", "F");
    let err = verify_private_dir(&dir).unwrap_err();
    assert!(
        matches!(err, DirAclError::WritableByOthers { ref sid } if sid == "S-1-1-0"),
        "{err}"
    );
}

#[test]
fn a_read_only_grant_to_others_is_accepted() {
    let (_parent, dir) = fresh_private_dir();
    grant(&dir, "*S-1-1-0", "R");
    verify_private_dir(&dir).expect("read access for others is not write access");
}

#[test]
fn a_user_modify_grant_to_authenticated_users_is_refused() {
    let (_parent, dir) = fresh_private_dir();
    grant(&dir, "*S-1-5-11", "M");
    let err = verify_private_dir(&dir).unwrap_err();
    assert!(matches!(err, DirAclError::WritableByOthers { .. }), "{err}");
}

#[test]
fn create_private_dir_makes_a_directory_that_verifies() {
    let parent = tempfile::tempdir().unwrap();
    let dir = parent.path().join("data");

    let created = create_private_dir(&dir).unwrap();

    assert!(created);
    assert!(dir.is_dir());
    verify_private_dir(&dir).unwrap();
    // Idempotent: an existing directory is left alone.
    assert!(!create_private_dir(&dir).unwrap());
}

#[test]
fn create_private_dir_does_not_inherit_a_widened_parent() {
    let parent = tempfile::tempdir().unwrap();
    grant(parent.path(), "*S-1-1-0", "F");
    let dir = parent.path().join("data");

    create_private_dir(&dir).unwrap();

    verify_private_dir(&dir).expect("the protected DACL ignores the parent's Everyone ACE");
}

#[test]
fn a_missing_dir_is_an_error() {
    let parent = tempfile::tempdir().unwrap();
    let err = verify_private_dir(&parent.path().join("nope")).unwrap_err();
    assert!(matches!(err, DirAclError::Platform(_)), "{err}");
}

#[test]
fn a_dir_owned_by_another_principal_is_refused() {
    // The Windows directory is owned by TrustedInstaller or SYSTEM, never by the test user.
    let windows = std::env::var_os("SystemRoot").expect("SystemRoot is set");
    let err = verify_private_dir(Path::new(&windows)).unwrap_err();
    assert!(matches!(err, DirAclError::NotOwnedByCurrentUser), "{err}");
}
