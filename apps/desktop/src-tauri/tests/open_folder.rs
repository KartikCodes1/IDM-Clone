//! `show_in_folder`: the launch plan is built and checked without running Explorer.

#![allow(clippy::expect_used)] // test code may expect (ARCHITECTURE 1.4)

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use forgedm_desktop_lib::error::CommandErrorKind;
use forgedm_desktop_lib::open_folder::{explorer_program, plan_launch, strip_verbatim_prefix};
use forgedm_protocol::{DownloadDetail, DownloadState, DownloadSummary};

const WINDOWS_ROOT: &str = r"C:\Windows";

fn golden_summary() -> DownloadSummary {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../crates/forgedm-protocol/tests/golden/v1/response_GetDownload.json");
    let text = std::fs::read_to_string(path).expect("golden readable");
    let value: serde_json::Value = serde_json::from_str(&text).expect("golden is JSON");
    let detail: DownloadDetail = serde_json::from_value(value["result"].clone()).expect("detail");
    detail.summary
}

fn completed_with(path: &Path) -> DownloadSummary {
    DownloadSummary {
        state: DownloadState::Completed,
        final_path: Some(path.to_string_lossy().into_owned()),
        ..golden_summary()
    }
}

fn temp_file(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("forgedm-open-folder-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join("done file.bin");
    std::fs::write(&file, b"x").expect("temp file");
    file
}

#[test]
fn explorer_is_the_system_root_executable() {
    assert_eq!(
        explorer_program(Path::new(WINDOWS_ROOT)),
        PathBuf::from(r"C:\Windows\explorer.exe")
    );
}

#[test]
fn launch_is_select_comma_then_the_canonical_path_and_nothing_else() {
    let file = temp_file("args");
    let plan = plan_launch(&completed_with(&file), Path::new(WINDOWS_ROOT)).expect("plans");
    let canonical = strip_verbatim_prefix(std::fs::canonicalize(&file).expect("canonical"));
    assert_eq!(plan.program, PathBuf::from(r"C:\Windows\explorer.exe"));
    assert_eq!(
        plan.args,
        vec![
            OsString::from("/select,"),
            canonical.clone().into_os_string()
        ]
    );
    assert!(
        !canonical.to_string_lossy().starts_with(r"\\?\"),
        "{canonical:?}"
    );
    let _ = std::fs::remove_dir_all(file.parent().expect("parent"));
}

#[test]
fn only_completed_downloads_can_be_shown() {
    let file = temp_file("state");
    for state in [
        DownloadState::Downloading,
        DownloadState::Failed,
        DownloadState::Paused,
    ] {
        let summary = DownloadSummary {
            state,
            ..completed_with(&file)
        };
        let err = plan_launch(&summary, Path::new(WINDOWS_ROOT)).expect_err("not completed");
        assert_eq!(err.kind, CommandErrorKind::InvalidRequest, "{state:?}");
    }
    let _ = std::fs::remove_dir_all(file.parent().expect("parent"));
}

#[test]
fn missing_final_path_or_missing_file_is_a_friendly_error() {
    let no_path = DownloadSummary {
        final_path: None,
        ..completed_with(Path::new("x"))
    };
    let err = plan_launch(&no_path, Path::new(WINDOWS_ROOT)).expect_err("no path");
    assert_eq!(err.kind, CommandErrorKind::InvalidRequest);

    let gone = completed_with(Path::new(r"C:\forgedm-test-no-such-dir\nothing.bin"));
    let err = plan_launch(&gone, Path::new(WINDOWS_ROOT)).expect_err("file missing");
    assert_eq!(err.kind, CommandErrorKind::InvalidRequest);
    assert!(err.message.contains("no longer"), "{}", err.message);
    assert!(!err.message.contains("nothing.bin"), "paths are not echoed");
}

#[test]
fn directories_are_not_shown() {
    let dir = std::env::temp_dir();
    let err = plan_launch(&completed_with(&dir), Path::new(WINDOWS_ROOT)).expect_err("a folder");
    assert_eq!(err.kind, CommandErrorKind::InvalidRequest);
}

#[test]
fn verbatim_prefix_is_stripped_only_for_drive_paths() {
    assert_eq!(
        strip_verbatim_prefix(PathBuf::from(r"\\?\C:\Users\me\a.bin")),
        PathBuf::from(r"C:\Users\me\a.bin")
    );
    assert_eq!(
        strip_verbatim_prefix(PathBuf::from(r"C:\plain\a.bin")),
        PathBuf::from(r"C:\plain\a.bin")
    );
    let unc = PathBuf::from(r"\\?\UNC\server\share\a.bin");
    assert_eq!(
        strip_verbatim_prefix(unc.clone()),
        unc,
        "UNC verbatim is left alone"
    );
}
