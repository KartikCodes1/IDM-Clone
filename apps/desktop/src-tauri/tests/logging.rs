//! Desktop logging: rolling file sink, filter default, failure fallback, and the removal of
//! the `whoami.exe` subprocess (W3 findings S0.10-rust-reviewer-7, ARCHITECTURE 8 and 3.7).

#![allow(clippy::expect_used)] // test code may expect (ARCHITECTURE 1.4)

use std::path::{Path, PathBuf};

use forgedm_desktop_lib::logging::{
    LOG_FILE_PREFIX, LOG_FILES_KEPT, LOG_SUBDIR, LoggingError, default_filter, install_global,
    log_dir_for, open_file_sink, subscriber_with,
};

fn unique_dir(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("forgedm-desktop-log-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn log_text(dir: &Path) -> String {
    std::fs::read_dir(dir)
        .expect("log dir exists")
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with(LOG_FILE_PREFIX))
        .map(|e| std::fs::read_to_string(e.path()).expect("log readable"))
        .collect()
}

#[test]
fn log_location_and_retention_follow_the_architecture() {
    assert_eq!(LOG_FILE_PREFIX, "desktop");
    assert_eq!(LOG_FILES_KEPT, 14);
    assert_eq!(
        log_dir_for(Path::new(r"C:\data\ForgeDM")),
        Path::new(r"C:\data\ForgeDM").join(LOG_SUBDIR)
    );
    assert_eq!(LOG_SUBDIR, "logs");
}

#[test]
fn file_sink_writes_prefixed_file_and_filters_below_info() {
    let dir = unique_dir("sink");
    let (writer, guard) = open_file_sink(&dir).expect("sink opens and creates the folder");
    let subscriber = subscriber_with(default_filter(None), writer);
    tracing::subscriber::with_default(subscriber, || {
        tracing::info!("startup-marker");
        tracing::debug!("debug-marker");
    });
    drop(guard); // flushes the background writer

    let text = log_text(&dir);
    assert!(text.contains("startup-marker"), "info is written: {text}");
    let first = text.lines().next().expect("one line written");
    let json: serde_json::Value = serde_json::from_str(first).expect("line is JSON");
    assert_eq!(json["fields"]["message"], "startup-marker");
    assert!(
        !text.contains("debug-marker"),
        "debug is filtered by default"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn explicit_filter_directive_overrides_the_default() {
    let dir = unique_dir("filter");
    let (writer, guard) = open_file_sink(&dir).expect("sink opens");
    let subscriber = subscriber_with(default_filter(Some("debug")), writer);
    tracing::subscriber::with_default(subscriber, || tracing::debug!("debug-marker"));
    drop(guard);
    assert!(log_text(&dir).contains("debug-marker"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unusable_log_folder_is_an_error_not_a_panic() {
    let blocker = unique_dir("blocked");
    std::fs::write(&blocker, b"a file, not a folder").expect("create blocker file");
    let result = open_file_sink(&blocker.join("logs"));
    assert!(result.is_err(), "a file in the way must be reported");
    let _ = std::fs::remove_file(&blocker);
}

#[test]
fn second_global_install_is_reported_not_swallowed() {
    // The first install may or may not win (other tests run in this process); the second
    // call must see a subscriber and report it.
    let dir = unique_dir("global");
    let _ = tracing::subscriber::set_global_default(subscriber_with(
        default_filter(None),
        std::io::sink,
    ));
    let (writer, guard) = open_file_sink(&dir).expect("sink opens");
    let err = install_global(default_filter(None), writer).expect_err("already installed");
    assert!(matches!(err, LoggingError::AlreadyInitialised));
    drop(guard);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn invalid_filter_directive_falls_back_to_info() {
    let filter = default_filter(Some("=== not a directive ==="));
    assert_eq!(
        filter.max_level_hint(),
        Some(tracing::level_filters::LevelFilter::INFO)
    );
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("dir readable").flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn desktop_code_spawns_no_subprocess() {
    let mut files = Vec::new();
    rust_sources(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    assert!(!files.is_empty(), "sources found");
    let spawn_apis = ["process::Command", "process::{", "tokio::process", "whoami"];
    for path in files {
        let text = std::fs::read_to_string(&path).expect("source readable");
        // Strip comments so prose may mention the forbidden names.
        let code: String = text
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join(
                "
",
            );
        for api in spawn_apis {
            assert!(!code.contains(api), "{path:?} uses {api}");
        }
        assert!(!code.contains("Command::new"), "{path:?} spawns a process");
    }
}
