//! Migrations, pragmas, version check and file hygiene.

mod common;

use std::sync::Arc;

use common::{TestSealer, open_temp};
use forgedm_storage::Storage;

#[tokio::test]
async fn migrations_apply_fresh_and_idempotent() {
    let db = open_temp().await;
    assert_eq!(db.storage.schema_version().await.unwrap(), 1);
    db.storage.close().await;

    // Re-opening an already migrated database applies nothing and keeps the version.
    let again = Storage::open(&db.path, Arc::new(TestSealer)).await.unwrap();
    assert_eq!(again.schema_version().await.unwrap(), 1);
    let tables = again.table_names().await.unwrap();
    for expected in ["downloads", "segments", "download_events", "settings"] {
        assert!(
            tables.iter().any(|t| t == expected),
            "missing table {expected}"
        );
    }
}

#[tokio::test]
async fn pragmas_wal_full_fk_busy() {
    let db = open_temp().await;
    let report = db.storage.pragmas().await.unwrap();
    assert_eq!(report.journal_mode, "wal");
    assert_eq!(report.synchronous, 2, "FULL");
    assert_eq!(report.foreign_keys, 1);
    assert_eq!(report.busy_timeout_ms, 5000);
    assert_eq!(report.journal_size_limit, 64 * 1024 * 1024);
    assert_eq!(report.reader_query_only, 1);
    assert_eq!(report.reader_synchronous, 2);
}

#[tokio::test]
async fn sqlite_version_at_least_3_37() {
    let db = open_temp().await;
    let version = db.storage.sqlite_version();
    let parts: Vec<u32> = version.split('.').map(|p| p.parse().unwrap()).collect();
    assert!(parts >= vec![3, 37, 0], "sqlite {version}");

    // The pure comparison used by the startup check.
    assert!(forgedm_storage::sqlite_version_at_least(
        "3.37.0",
        (3, 37, 0)
    ));
    assert!(forgedm_storage::sqlite_version_at_least(
        "3.50.4",
        (3, 37, 0)
    ));
    assert!(forgedm_storage::sqlite_version_at_least(
        "4.0.0",
        (3, 37, 0)
    ));
    assert!(!forgedm_storage::sqlite_version_at_least(
        "3.36.9",
        (3, 37, 0)
    ));
    assert!(!forgedm_storage::sqlite_version_at_least(
        "garbage",
        (3, 37, 0)
    ));
}

#[test]
fn migration_files_have_no_crlf() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let mut seen = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "sql") {
            let bytes = std::fs::read(&path).unwrap();
            assert!(!bytes.contains(&b'\r'), "CR byte in {}", path.display());
            seen += 1;
        }
    }
    assert!(seen >= 1, "no migration files found");
}

#[tokio::test]
async fn corrupt_db_typed_error_file_bytes_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("forgedm.db");
    let garbage: Vec<u8> = (0..8192u32)
        .map(|i| u8::try_from(i.wrapping_mul(31) % 251).unwrap())
        .collect();
    std::fs::write(&path, &garbage).unwrap();

    let err = Storage::open(&path, Arc::new(TestSealer))
        .await
        .unwrap_err();
    assert!(err.is_corrupt(), "expected Corrupt, got {err:?}");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        garbage,
        "file must be preserved"
    );
}
