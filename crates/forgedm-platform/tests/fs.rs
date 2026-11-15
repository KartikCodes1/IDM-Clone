//! Filesystem primitives (S1.5): sparse files, ids, no-replace move, finalize, path safety.
#![allow(clippy::expect_used, clippy::print_stderr)] // test helpers; NOT RUN notes go to stderr

use std::cell::{Cell, RefCell};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::windows::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use forgedm_platform::PlatformError;
use forgedm_platform::fs::{
    IoClass, allocated_size, classify_io, create_part_file, delete_by_handle, ensure_within,
    file_id, final_path, finalize_move, free_space, is_name_surrogate, move_no_replace,
    open_part_file, reparse_tag_is_name_surrogate, set_sparse, volume_info,
};

const ATTR_SPARSE: u32 = 0x200;
const PROBE_ENV: &str = "FORGEDM_PART_PROBE_PATH";
const PROBE_OK_CODE: i32 = 42;
const GIB: u64 = 1 << 30;

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

fn write_file(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).expect("write file");
}

/// Creates a directory junction (`mklink /J`, no admin needed).
fn make_junction(link: &Path, target: &Path) {
    let out = Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(link)
        .arg(target)
        .output()
        .expect("run cmd");
    assert!(
        out.status.success(),
        "mklink /J failed: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// Returns false (and says NOT RUN) when symlinks cannot be created; else leaves
/// `probe_link` -> `probe_target` in `dir`.
fn symlink_privilege_available(dir: &Path) -> bool {
    let target = dir.join("probe_target");
    write_file(&target, b"x");
    match std::os::windows::fs::symlink_file(&target, dir.join("probe_link")) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("NOT RUN: symlink privilege unavailable ({e})");
            false
        }
    }
}

fn create_rw_new(path: &Path) -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
        .expect("create")
}

fn block_rename(path: &Path) -> File {
    // Read-only sharing (no FILE_SHARE_DELETE) makes a rename fail with a sharing violation.
    OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(path)
        .expect("blocker")
}

#[test]
fn sparse_5gib_logical_small_allocation_attr_0x200() {
    let dir = tmp();
    let path = dir.path().join("big.bin");
    let file = create_rw_new(&path);

    set_sparse(&file, true).expect("set sparse");
    let tail = [0xABu8; 4096];
    file.seek_write(&tail, 5 * GIB - 4096).expect("far write");

    let meta = file.metadata().expect("metadata");
    assert_eq!(meta.len(), 5 * GIB);
    assert_ne!(meta.file_attributes() & ATTR_SPARSE, 0, "sparse attribute");
    let allocated = allocated_size(&path).expect("allocated size");
    assert!(allocated < 4 * 1024 * 1024, "allocated {allocated}");
}

#[test]
fn clear_sparse_after_full_write() {
    let dir = tmp();
    let path = dir.path().join("full.bin");
    let mut file = create_rw_new(&path);
    set_sparse(&file, true).expect("set sparse");
    let data: Vec<u8> = (0..1_048_576u32)
        .map(|i| u8::try_from(i % 251).unwrap_or(0))
        .collect();
    file.write_all(&data).expect("write");
    file.sync_all().expect("sync");

    set_sparse(&file, false).expect("clear sparse");

    let attrs = file.metadata().expect("metadata").file_attributes();
    assert_eq!(attrs & ATTR_SPARSE, 0, "sparse attribute cleared");
    drop(file);
    assert_eq!(fs::read(&path).expect("read back"), data);
}

#[test]
fn part_file_id_survives_same_volume_rename() {
    let dir = tmp();
    let part = dir.path().join("a.bin.deadbeef.forgedm.part");
    let fin = dir.path().join("a.bin");
    let file = create_part_file(&part).expect("create part");
    let before = file_id(&file).expect("id before");
    drop(file); // an open handle without FILE_SHARE_DELETE would block the rename

    move_no_replace(&part, &fin).expect("move");

    let after = file_id(&File::open(&fin).expect("open final")).expect("id after");
    assert_eq!(before, after);
}

#[test]
fn no_replace_collision_already_exists() {
    let dir = tmp();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    write_file(&src, b"new");
    write_file(&dst, b"precious");

    let err = move_no_replace(&src, &dst).expect_err("must refuse");

    assert!(matches!(err, PlatformError::AlreadyExists), "{err:?}");
    assert_eq!(fs::read(&dst).expect("dst"), b"precious");
    assert_eq!(fs::read(&src).expect("src"), b"new");
}

#[test]
fn collision_suffix_picks_next_free_existing_untouched() {
    let dir = tmp();
    let src = dir.path().join("src.part");
    write_file(&src, b"payload");
    write_file(&dir.path().join("report.pdf"), b"one");
    write_file(&dir.path().join("report (1).pdf"), b"two");

    let got = finalize_move(
        &src,
        dir.path(),
        "report.pdf",
        &mut |_c| Ok(()),
        Duration::from_secs(10),
        &mut |_d| {},
    )
    .expect("finalize");

    assert_eq!(got, dir.path().join("report (2).pdf"));
    assert_eq!(fs::read(&got).expect("final"), b"payload");
    assert_eq!(fs::read(dir.path().join("report.pdf")).expect("r0"), b"one");
    assert_eq!(
        fs::read(dir.path().join("report (1).pdf")).expect("r1"),
        b"two"
    );
    assert!(!src.exists());
}

#[test]
fn finalize_move_reports_each_candidate_before_attempt() {
    let dir = tmp();
    let src = dir.path().join("src.part");
    write_file(&src, b"payload");
    write_file(&dir.path().join("noext"), b"a");
    write_file(&dir.path().join("noext (1)"), b"b");
    let seen: RefCell<Vec<PathBuf>> = RefCell::new(Vec::new());

    let mut on_candidate = |c: &Path| {
        // Before the attempt the source is still in place; the winner is not yet created.
        assert!(src.exists(), "source must still exist at {c:?}");
        seen.borrow_mut().push(c.to_path_buf());
        Ok(())
    };
    let got = finalize_move(
        &src,
        dir.path(),
        "noext",
        &mut on_candidate,
        Duration::from_secs(10),
        &mut |_d| {},
    )
    .expect("finalize");

    let expected: Vec<PathBuf> = ["noext", "noext (1)", "noext (2)"]
        .iter()
        .map(|n| dir.path().join(n))
        .collect();
    assert_eq!(*seen.borrow(), expected);
    assert_eq!(got, expected[2]);
    assert!(got.exists());
}

#[test]
fn finalize_move_aborts_when_candidate_persist_fails() {
    let dir = tmp();
    let src = dir.path().join("src.part");
    write_file(&src, b"payload");

    let err = finalize_move(
        &src,
        dir.path(),
        "x.bin",
        &mut |_c| Err(PlatformError::InvalidInput("persist failed")),
        Duration::from_secs(10),
        &mut |_d| {},
    )
    .expect_err("must abort");

    assert!(matches!(err, PlatformError::InvalidInput(_)), "{err:?}");
    assert!(src.exists());
    assert!(!dir.path().join("x.bin").exists());
}

#[test]
fn sharing_violation_retried_then_succeeds() {
    let dir = tmp();
    let src = dir.path().join("src.part");
    write_file(&src, b"payload");
    let blocker = RefCell::new(Some(block_rename(&src)));
    let sleeps: RefCell<Vec<Duration>> = RefCell::new(Vec::new());

    let mut sleeper = |d: Duration| {
        sleeps.borrow_mut().push(d);
        if sleeps.borrow().len() == 2 {
            blocker.borrow_mut().take(); // release the lock
        }
    };
    let got = finalize_move(
        &src,
        dir.path(),
        "ok.bin",
        &mut |_c| Ok(()),
        Duration::from_secs(10),
        &mut sleeper,
    )
    .expect("finalize after retries");

    assert_eq!(got, dir.path().join("ok.bin"));
    let sleeps = sleeps.borrow();
    assert_eq!(sleeps.len(), 2, "{sleeps:?}");
    assert_eq!(sleeps[0], Duration::from_millis(100));
    assert!(sleeps[1] > sleeps[0], "backoff grows: {sleeps:?}");
}

#[test]
fn retry_budget_exhausts() {
    let dir = tmp();
    let src = dir.path().join("src.part");
    write_file(&src, b"payload");
    let _blocker = block_rename(&src);
    let total = Cell::new(Duration::ZERO);

    let mut sleeper = |d: Duration| total.set(total.get() + d);
    let err = finalize_move(
        &src,
        dir.path(),
        "never.bin",
        &mut |_c| Ok(()),
        Duration::from_secs(10),
        &mut sleeper,
    )
    .expect_err("must give up");

    assert!(matches!(err, PlatformError::SharingViolation), "{err:?}");
    assert_eq!(
        total.get(),
        Duration::from_secs(10),
        "whole budget used, none exceeded"
    );
    assert!(src.exists());
    assert!(!dir.path().join("never.bin").exists());
}

#[test]
fn long_path_over_260() {
    let dir = tmp();
    let mut long = dir.path().to_path_buf();
    for _ in 0..3 {
        long.push("d".repeat(90));
    }
    fs::create_dir_all(&long).expect("create long dir");
    let src = long.join("source.part");
    let dst = long.join("final.bin");
    write_file(&src, b"long");
    assert!(dst.as_os_str().len() > 260, "{}", dst.as_os_str().len());

    move_no_replace(&src, &dst).expect("move over long path");

    assert_eq!(fs::read(&dst).expect("dst"), b"long");
    assert!(free_space(&long).expect("free space on long dir") > 0);
}

#[test]
fn free_space_nonzero() {
    let dir = tmp();
    assert!(free_space(dir.path()).expect("free space") > 0);
}

#[test]
fn volume_info_reports_ntfs_sparse_and_streams() {
    let dir = tmp();
    let file = File::create(dir.path().join("v.bin")).expect("create");

    let info = volume_info(&file).expect("volume info");

    assert_eq!(info.fs_name, "NTFS");
    assert!(info.supports_sparse);
    assert!(info.supports_streams);
}

#[test]
fn final_path_and_ensure_within_accept_inside_reject_outside() {
    let dir = tmp();
    let root = fs::canonicalize(dir.path()).expect("canonical");
    let other = tmp();
    let inside = create_part_file(&dir.path().join("in.part")).expect("create");
    let outside = create_part_file(&other.path().join("out.part")).expect("create");

    assert!(final_path(&inside).expect("final path").starts_with(&root));
    ensure_within(&inside, &root).expect("inside is within");
    let err = ensure_within(&outside, &root).expect_err("outside rejected");
    assert!(matches!(err, PlatformError::PathEscape), "{err:?}");
}

#[test]
fn ensure_within_requires_a_path_boundary() {
    let dir = tmp();
    let root = dir.path().join("dl");
    let sibling = dir.path().join("dl-evil");
    fs::create_dir(&root).expect("root");
    fs::create_dir(&sibling).expect("sibling");
    let canon_root = fs::canonicalize(&root).expect("canonical");
    let file = create_part_file(&sibling.join("x.part")).expect("create");

    let err = ensure_within(&file, &canon_root).expect_err("prefix sibling rejected");

    assert!(matches!(err, PlatformError::PathEscape), "{err:?}");
}

#[test]
fn junction_escape_detected() {
    let dir = tmp();
    let target = dir.path().join("target");
    let outside = dir.path().join("outside");
    fs::create_dir(&target).expect("target");
    fs::create_dir(&outside).expect("outside");
    make_junction(&target.join("link"), &outside);
    let root = fs::canonicalize(&target).expect("canonical target");

    // The create follows the junction directory component and lands outside the target.
    let file = create_part_file(&target.join("link").join("x.part")).expect("create via junction");

    let err = ensure_within(&file, &root).expect_err("escape detected");
    assert!(matches!(err, PlatformError::PathEscape), "{err:?}");
    delete_by_handle(&file).expect("delete our own file by handle");
    drop(file);
    assert!(!outside.join("x.part").exists(), "escaped file removed");
}

#[test]
fn create_new_fails_on_planted_symlink() {
    let dir = tmp();
    if !symlink_privilege_available(dir.path()) {
        return; // NOT RUN: recorded in the slice report.
    }
    let victim = dir.path().join("victim.txt");
    write_file(&victim, b"victim");
    let part = dir.path().join("a.part");
    std::os::windows::fs::symlink_file(&victim, &part).expect("plant symlink");

    let err = create_part_file(&part).expect_err("planted link must fail");

    assert!(matches!(err, PlatformError::AlreadyExists), "{err:?}");
    assert_eq!(fs::read(&victim).expect("victim"), b"victim");
}

#[test]
fn name_surrogate_detection() {
    let dir = tmp();
    let plain_dir = dir.path().join("plain");
    let plain_file = dir.path().join("plain.txt");
    let junction = dir.path().join("junc");
    fs::create_dir(&plain_dir).expect("dir");
    write_file(&plain_file, b"x");
    make_junction(&junction, &plain_dir);

    assert!(!is_name_surrogate(&plain_dir).expect("dir"));
    assert!(!is_name_surrogate(&plain_file).expect("file"));
    assert!(is_name_surrogate(&junction).expect("junction"));

    // Reparse tags: symlink and junction are name surrogates, cloud placeholders are not.
    assert!(reparse_tag_is_name_surrogate(0xA000_000C), "symlink");
    assert!(reparse_tag_is_name_surrogate(0xA000_0003), "mount point");
    assert!(!reparse_tag_is_name_surrogate(0x9000_001A), "cloud");
    assert!(!reparse_tag_is_name_surrogate(0x9000_701A), "cloud 7");
    assert!(!reparse_tag_is_name_surrogate(0x8000_0017), "wof");

    if symlink_privilege_available(dir.path()) {
        assert!(is_name_surrogate(&dir.path().join("probe_link")).expect("symlink"));
    }
}

#[test]
fn disk_full_codes_classified() {
    let class = |code: i32| classify_io(&std::io::Error::from_raw_os_error(code));

    assert_eq!(class(112), IoClass::DiskFull);
    assert_eq!(class(39), IoClass::DiskFull);
    assert_eq!(class(21), IoClass::DeviceGone);
    assert_eq!(class(1167), IoClass::DeviceGone);
    assert_eq!(class(5), IoClass::AccessDenied);
    assert_eq!(class(32), IoClass::Sharing);
    assert_eq!(class(2), IoClass::Other);
    assert_eq!(
        classify_io(&std::io::Error::other("no os code")),
        IoClass::Other
    );
}

#[test]
fn part_share_mode_read_only_for_others() {
    let dir = tmp();
    let part = dir.path().join("p.bin.cafebabe.forgedm.part");
    let mut file = create_part_file(&part).expect("create part");
    file.write_all(b"data").expect("write");

    let exe = std::env::current_exe().expect("current exe");
    let status = Command::new(exe)
        .args(["--exact", "part_share_child_probe", "--nocapture"])
        .env(PROBE_ENV, &part)
        .status()
        .expect("spawn probe process");

    assert_eq!(status.code(), Some(PROBE_OK_CODE), "probe: {status:?}");
    let mut back = String::new();
    File::open(&part)
        .expect("reopen")
        .read_to_string(&mut back)
        .expect("read");
    assert_eq!(back, "data");
}

/// Child half of `part_share_mode_read_only_for_others`: runs only when spawned with the env
/// var set. Exits with [`PROBE_OK_CODE`] when every expectation holds.
#[test]
fn part_share_child_probe() {
    let Some(path) = std::env::var_os(PROBE_ENV) else {
        return;
    };
    let path = PathBuf::from(path);
    let sharing = |e: &std::io::Error| e.raw_os_error() == Some(32);
    let read_ok = OpenOptions::new()
        .read(true)
        .share_mode(7)
        .open(&path)
        .is_ok();
    let write_err = OpenOptions::new()
        .write(true)
        .share_mode(7)
        .open(&path)
        .err();
    let delete_err = OpenOptions::new()
        .access_mode(0x0001_0000) // DELETE
        .share_mode(7)
        .open(&path)
        .err();
    let remove_err = fs::remove_file(&path).err();

    let ok = read_ok
        && write_err.as_ref().is_some_and(sharing)
        && delete_err.as_ref().is_some_and(sharing)
        && remove_err.is_some();
    std::process::exit(if ok { PROBE_OK_CODE } else { 1 });
}

#[test]
fn finalize_rejects_names_with_separators_or_streams() {
    let dir = tmp();
    let src = dir.path().join("src.part");
    write_file(&src, b"payload");

    for bad in ["", "..", "a\\b", "a/b", "a:stream", "a\0b"] {
        let err = finalize_move(
            &src,
            dir.path(),
            bad,
            &mut |_c| Ok(()),
            Duration::from_secs(1),
            &mut |_d| {},
        )
        .expect_err("bad name");
        assert!(
            matches!(err, PlatformError::InvalidInput(_)),
            "{bad:?}: {err:?}"
        );
    }
    assert!(src.exists());
}

#[test]
fn finalize_rejects_win32_unsafe_names() {
    let dir = tmp();
    let src = dir.path().join("src.part");
    write_file(&src, b"payload");
    let too_long = "a".repeat(234);

    let mut bad: Vec<&str> = vec![
        "CON", "nul", "Aux.txt", "COM1", "lpt9.log", "COM0", "CONIN$", "CONOUT$", "con .txt", "a.",
        "a ", "a<b", "a>b", "a\"b", "a|b", "a?b", "a*b", "a\u{1}b", "a\u{1f}b",
    ];
    bad.push(&too_long);
    for name in bad {
        let err = finalize_move(
            &src,
            dir.path(),
            name,
            &mut |_c| Ok(()),
            Duration::from_secs(1),
            &mut |_d| {},
        )
        .expect_err("unsafe name");
        assert!(
            matches!(err, PlatformError::InvalidInput(_)),
            "{name:?}: {err:?}"
        );
    }
    assert!(src.exists());
}

#[test]
fn finalize_accepts_boundary_and_lookalike_names() {
    let dir = tmp();
    let longest = "a".repeat(233);
    for (i, name) in [
        longest.as_str(),
        "COM10",
        "CONSOLE.txt",
        "com.txt",
        ".hidden",
    ]
    .into_iter()
    .enumerate()
    {
        let src = dir.path().join(format!("s{i}.part"));
        write_file(&src, b"x");
        let got = finalize_move(
            &src,
            dir.path(),
            name,
            &mut |_c| Ok(()),
            Duration::from_secs(1),
            &mut |_d| {},
        )
        .expect("name is accepted");
        assert!(got.exists(), "{name:?}");
    }
}

#[test]
fn open_part_file_reopens_regular_and_refuses_symlink() {
    let dir = tmp();
    let part = dir.path().join("r.part");
    drop(create_part_file(&part).expect("create"));
    open_part_file(&part).expect("reopen regular file");

    if !symlink_privilege_available(dir.path()) {
        return; // NOT RUN: symlink part of this test
    }
    let err = open_part_file(&dir.path().join("probe_link")).expect_err("symlink refused");
    assert!(matches!(err, PlatformError::PathEscape), "{err:?}");
}
