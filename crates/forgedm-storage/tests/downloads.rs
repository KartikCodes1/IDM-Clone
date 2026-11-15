//! Download repository: round trip, enum validation, CAS transitions, list, injection.

mod common;

use std::sync::Arc;

use common::{FailingSealer, new_download, new_id, open_temp, raw_pool};
use forgedm_protocol::{DownloadState, TransferMode};
use forgedm_storage::{NewEvent, ProbeResult, Storage, StorageError};

const URL: &str = "https://example.test/file.bin?token=abc123";

#[tokio::test]
async fn download_roundtrip_with_test_sealer() {
    let db = open_temp().await;
    let id = new_id();
    db.storage
        .downloads()
        .insert(&new_download(id, URL))
        .await
        .unwrap();

    let record = db.storage.downloads().get(id).await.unwrap().unwrap();
    assert_eq!(record.id, id);
    assert_eq!(record.state, DownloadState::Queued);
    assert_eq!(record.url_display, "https://example.test/file.bin");
    assert_eq!(record.max_connections, 8);
    assert_eq!(record.bytes_committed, 0);
    assert_eq!(
        db.storage
            .downloads()
            .original_url(id)
            .await
            .unwrap()
            .expose(),
        URL
    );
    assert!(
        db.storage
            .downloads()
            .final_url(id)
            .await
            .unwrap()
            .is_none()
    );

    let probe = ProbeResult {
        transfer_mode: Some(TransferMode::Segmented),
        total_size: Some(4096),
        final_url: Some("https://cdn.test/x?sig=zzz".to_owned()),
        etag: Some("\"v1\"".to_owned()),
        etag_strong: Some(true),
        ..ProbeResult::default()
    };
    db.storage
        .downloads()
        .save_probe(id, &probe, 5)
        .await
        .unwrap();
    let record = db.storage.downloads().get(id).await.unwrap().unwrap();
    assert_eq!(record.transfer_mode, Some(TransferMode::Segmented));
    assert_eq!(record.total_size, Some(4096));
    assert_eq!(record.etag_strong, Some(true));
    let final_url = db.storage.downloads().final_url(id).await.unwrap().unwrap();
    assert_eq!(final_url.expose(), "https://cdn.test/x?sig=zzz");
    assert!(
        !format!("{final_url:?}").contains("zzz"),
        "Debug must redact"
    );

    assert!(
        db.storage
            .downloads()
            .get(new_id())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn seal_failure_inserts_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("forgedm.db");
    let storage = Storage::open(&path, Arc::new(FailingSealer)).await.unwrap();
    let id = new_id();
    let err = storage
        .downloads()
        .insert(&new_download(id, URL))
        .await
        .unwrap_err();
    assert!(matches!(err, StorageError::Crypto(_)), "{err:?}");
    assert!(storage.downloads().get(id).await.unwrap().is_none());
}

#[tokio::test]
async fn wrong_row_entropy_fails() {
    let db = open_temp().await;
    let (a, b) = (new_id(), new_id());
    db.storage
        .downloads()
        .insert(&new_download(a, URL))
        .await
        .unwrap();
    db.storage
        .downloads()
        .insert(&new_download(b, URL))
        .await
        .unwrap();

    let raw = raw_pool(&db.path).await;
    // Copy A's sealed blob onto B (other row, same column).
    sqlx::query(
        "UPDATE downloads SET original_url_sealed = \
         (SELECT original_url_sealed FROM downloads WHERE id = ?1) WHERE id = ?2",
    )
    .bind(a.to_string())
    .bind(b.to_string())
    .execute(&raw)
    .await
    .unwrap();
    let err = db.storage.downloads().original_url(b).await.unwrap_err();
    assert!(matches!(err, StorageError::Crypto(_)), "row swap: {err:?}");
    assert_eq!(
        db.storage
            .downloads()
            .original_url(a)
            .await
            .unwrap()
            .expose(),
        URL
    );

    // Copy A's original-URL blob into A's own final-URL column (same row, other column).
    sqlx::query("UPDATE downloads SET final_url_sealed = original_url_sealed WHERE id = ?1")
        .bind(a.to_string())
        .execute(&raw)
        .await
        .unwrap();
    let err = db.storage.downloads().final_url(a).await.unwrap_err();
    assert!(
        matches!(err, StorageError::Crypto(_)),
        "column swap: {err:?}"
    );
}

#[tokio::test]
async fn unseal_failure_is_typed_crypto_error() {
    let db = open_temp().await;
    let id = new_id();
    db.storage
        .downloads()
        .insert(&new_download(id, URL))
        .await
        .unwrap();

    let raw = raw_pool(&db.path).await;
    sqlx::query("UPDATE downloads SET original_url_sealed = X'0102030405' WHERE id = ?1")
        .bind(id.to_string())
        .execute(&raw)
        .await
        .unwrap();
    let err = db.storage.downloads().original_url(id).await.unwrap_err();
    assert!(matches!(err, StorageError::Crypto(_)), "{err:?}");
    assert!(err.is_secrets_unavailable());
    assert!(
        !err.to_string().contains("abc123"),
        "error must not carry the secret"
    );

    // Listing and get never unseal, so a broken blob does not break them.
    assert_eq!(
        db.storage
            .downloads()
            .list(None, 10, 0)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(db.storage.downloads().get(id).await.unwrap().is_some());
}

#[tokio::test]
async fn unknown_state_string_rejected_on_load() {
    let db = open_temp().await;
    let id = new_id();
    db.storage
        .downloads()
        .insert(&new_download(id, URL))
        .await
        .unwrap();
    let raw = raw_pool(&db.path).await;
    sqlx::query("UPDATE downloads SET state = 'bogus' WHERE id = ?1")
        .bind(id.to_string())
        .execute(&raw)
        .await
        .unwrap();

    let err = db.storage.downloads().get(id).await.unwrap_err();
    assert!(
        matches!(&err, StorageError::UnknownEnumValue { column: "state", value } if value == "bogus"),
        "{err:?}"
    );
    assert!(db.storage.downloads().list(None, 10, 0).await.is_err());
}

#[tokio::test]
async fn transition_cas_conflict_on_stale_from_state() {
    let db = open_temp().await;
    let id = new_id();
    db.storage
        .downloads()
        .insert(&new_download(id, URL))
        .await
        .unwrap();
    let event = NewEvent::new(10, "state");

    db.storage
        .downloads()
        .transition(
            id,
            DownloadState::Queued,
            DownloadState::Probing,
            None,
            &event,
        )
        .await
        .unwrap();
    // A second writer still believing the download is queued loses.
    let err = db
        .storage
        .downloads()
        .transition(
            id,
            DownloadState::Queued,
            DownloadState::Paused,
            Some("user"),
            &event,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            StorageError::Conflict {
                expected: DownloadState::Queued,
                actual: Some(DownloadState::Probing),
                ..
            }
        ),
        "{err:?}"
    );
    let record = db.storage.downloads().get(id).await.unwrap().unwrap();
    assert_eq!(record.state, DownloadState::Probing);

    // The losing transition wrote no event; the winning one wrote exactly one.
    let events = db.storage.events().list(id, 50).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].from_state, Some(DownloadState::Queued));
    assert_eq!(events[0].to_state, Some(DownloadState::Probing));

    let missing = db
        .storage
        .downloads()
        .transition(
            new_id(),
            DownloadState::Queued,
            DownloadState::Probing,
            None,
            &event,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(missing, StorageError::NotFound { .. }),
        "{missing:?}"
    );

    let unknown = db
        .storage
        .downloads()
        .transition(
            id,
            DownloadState::Probing,
            DownloadState::Unknown,
            None,
            &event,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(unknown, StorageError::InvalidValue { .. }),
        "{unknown:?}"
    );
}

#[tokio::test]
async fn list_bounded_and_ordered() {
    let db = open_temp().await;
    let mut ids = Vec::new();
    for n in 0..620_i64 {
        let id = new_id();
        let mut d = new_download(id, URL);
        d.created_at_ms = 1_000 + n;
        db.storage.downloads().insert(&d).await.unwrap();
        ids.push(id);
    }
    // Newest first, and a request above LIST_PAGE_MAX is clamped to it.
    let page = db.storage.downloads().list(None, 10_000, 0).await.unwrap();
    assert_eq!(page.len(), 500);
    assert!(
        page.windows(2)
            .all(|w| w[0].created_at_ms >= w[1].created_at_ms)
    );
    assert_eq!(page[0].id, ids[619]);

    let second = db.storage.downloads().list(None, 500, 500).await.unwrap();
    assert_eq!(second.len(), 120);
    assert_eq!(second[119].id, ids[0]);
    assert!(
        db.storage
            .downloads()
            .list(None, 0, 0)
            .await
            .unwrap()
            .is_empty()
    );

    // State filter.
    let event = NewEvent::new(1, "state");
    db.storage
        .downloads()
        .transition(
            ids[3],
            DownloadState::Queued,
            DownloadState::Paused,
            Some("user"),
            &event,
        )
        .await
        .unwrap();
    let paused = db
        .storage
        .downloads()
        .list(Some(&[DownloadState::Paused]), 50, 0)
        .await
        .unwrap();
    assert_eq!(paused.len(), 1);
    assert_eq!(paused[0].id, ids[3]);
    assert_eq!(paused[0].state_reason.as_deref(), Some("user"));
    let none = db.storage.downloads().list(Some(&[]), 50, 0).await.unwrap();
    assert!(none.is_empty(), "empty filter matches nothing");
}

#[tokio::test]
async fn sql_metacharacters_inert() {
    let db = open_temp().await;
    let id = new_id();
    let nasty = "x'); DROP TABLE downloads; --";
    let mut d = new_download(id, &format!("https://e.test/{nasty}"));
    d.display_name = nasty.to_owned();
    d.target_dir = "C:\\a'b\"c%_\\".to_owned();
    d.url_display = "https://e.test/\u{1F4A5}z".to_owned();
    db.storage.downloads().insert(&d).await.unwrap();

    let record = db.storage.downloads().get(id).await.unwrap().unwrap();
    assert_eq!(record.display_name, nasty);
    assert_eq!(record.target_dir, d.target_dir);
    assert_eq!(record.url_display, d.url_display);
    let url = db.storage.downloads().original_url(id).await.unwrap();
    assert!(url.expose().ends_with(nasty));
    // The table is still there and holds exactly one row.
    assert_eq!(
        db.storage
            .downloads()
            .list(None, 10, 0)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn complete_set_finalize_target_and_delete() {
    let db = open_temp().await;
    let id = new_id();
    db.storage
        .downloads()
        .insert(&new_download(id, URL))
        .await
        .unwrap();
    // finalize_target and completed can only be reached through their guarded paths.
    let early_target = db
        .storage
        .downloads()
        .set_finalize_target(id, "C:\\d\\file (1).bin", 7)
        .await
        .unwrap_err();
    assert!(
        matches!(
            early_target,
            StorageError::Conflict {
                expected: DownloadState::Finalizing,
                ..
            }
        ),
        "{early_target:?}"
    );
    let bypass = db
        .storage
        .downloads()
        .transition(
            id,
            DownloadState::Queued,
            DownloadState::Completed,
            None,
            &NewEvent::new(6, "state"),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(bypass, StorageError::InvalidValue { field: "state" }),
        "{bypass:?}"
    );
    let record = db.storage.downloads().get(id).await.unwrap().unwrap();
    assert_eq!(
        record.state,
        DownloadState::Queued,
        "bypass changed nothing"
    );

    // complete() is a compare-and-set from `finalizing` only.
    let sha = "ab".repeat(32);
    let early = db
        .storage
        .downloads()
        .complete(id, "C:\\x", &sha, true, 98)
        .await
        .unwrap_err();
    assert!(matches!(
        early,
        StorageError::Conflict {
            expected: DownloadState::Finalizing,
            ..
        }
    ));
    let event = NewEvent::new(8, "state");
    db.storage
        .downloads()
        .transition(
            id,
            DownloadState::Queued,
            DownloadState::Finalizing,
            None,
            &event,
        )
        .await
        .unwrap();
    db.storage
        .downloads()
        .set_finalize_target(id, "C:\\d\\file (1).bin", 7)
        .await
        .unwrap();
    let record = db.storage.downloads().get(id).await.unwrap().unwrap();
    assert_eq!(
        record.finalize_target.as_deref(),
        Some("C:\\d\\file (1).bin")
    );

    // The checksum must be 64 lowercase hex characters.
    for bad in ["", "zz", &"AB".repeat(32), &"g".repeat(64)] {
        let err = db
            .storage
            .downloads()
            .complete(id, "C:\\x", bad, true, 98)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                StorageError::InvalidValue {
                    field: "checksum_actual"
                }
            ),
            "{bad:?}: {err:?}"
        );
    }
    db.storage
        .downloads()
        .complete(id, "C:\\d\\file (1).bin", &sha, true, 99)
        .await
        .unwrap();
    let record = db.storage.downloads().get(id).await.unwrap().unwrap();
    assert_eq!(record.state, DownloadState::Completed);
    assert_eq!(record.final_path.as_deref(), Some("C:\\d\\file (1).bin"));
    assert_eq!(record.completed_at_ms, Some(99));
    assert_eq!(record.length_verified, Some(true));
    assert_eq!(record.checksum_actual.as_deref(), Some(sha.as_str()));

    assert!(db.storage.downloads().delete(id).await.unwrap());
    assert!(
        !db.storage.downloads().delete(id).await.unwrap(),
        "repeat delete reports false"
    );
    assert!(db.storage.downloads().get(id).await.unwrap().is_none());
}

#[tokio::test]
async fn expected_checksum_is_a_column_that_survives_event_pruning() {
    let db = open_temp().await;
    let id = new_id();
    let sha = "ab".repeat(32);
    db.storage
        .downloads()
        .insert_with_checksum(&new_download(id, URL), Some(&sha))
        .await
        .unwrap();
    for n in 0..300 {
        db.storage
            .events()
            .append(id, &NewEvent::new(n, "noise"))
            .await
            .unwrap();
    }

    // A checkpoint prunes the event log to its retention limit.
    let segment = forgedm_storage::SegmentRecord {
        seg_no: 0,
        start: 0,
        end: None,
        committed: 10,
        retry_count: 0,
        updated_at_ms: 1,
    };
    db.storage
        .segments()
        .save_checkpoint(id, 10, &[segment])
        .await
        .unwrap();

    let events = db.storage.events().list(id, 500).await.unwrap();
    assert!(events.len() <= 200, "events were pruned: {}", events.len());
    let record = db.storage.downloads().get(id).await.unwrap().unwrap();
    assert_eq!(record.checksum_expected.as_deref(), Some(sha.as_str()));
}

#[tokio::test]
async fn a_malformed_expected_checksum_inserts_nothing() {
    let db = open_temp().await;
    let id = new_id();

    for bad in ["abc", &"AB".repeat(32), &"zz".repeat(32)] {
        let result = db
            .storage
            .downloads()
            .insert_with_checksum(&new_download(id, URL), Some(bad))
            .await;
        assert!(
            matches!(result, Err(StorageError::InvalidValue { .. })),
            "{bad}"
        );
    }
    assert!(db.storage.downloads().get(id).await.unwrap().is_none());
}

#[tokio::test]
async fn reset_for_restart_clears_progress_and_identity_but_keeps_state_and_checksum() {
    let db = open_temp().await;
    let id = new_id();
    let sha = "cd".repeat(32);
    let downloads = db.storage.downloads();
    downloads
        .insert_with_checksum(&new_download(id, URL), Some(&sha))
        .await
        .unwrap();
    let probe = ProbeResult {
        transfer_mode: Some(TransferMode::SingleRanged),
        total_size: Some(100),
        part_path: Some(r"C:\d\a.part".to_owned()),
        part_file_id: Some(vec![1, 2, 3]),
        etag: Some("\"v1\"".to_owned()),
        etag_strong: Some(true),
        ..ProbeResult::default()
    };
    downloads.save_probe(id, &probe, 5).await.unwrap();
    let segment = forgedm_storage::SegmentRecord {
        seg_no: 0,
        start: 0,
        end: Some(100),
        committed: 40,
        retry_count: 0,
        updated_at_ms: 6,
    };
    db.storage
        .segments()
        .save_checkpoint(id, 40, &[segment])
        .await
        .unwrap();

    downloads.reset_for_restart(id, 9).await.unwrap();

    let record = downloads.get(id).await.unwrap().unwrap();
    assert_eq!(record.bytes_committed, 0);
    assert_eq!(record.total_size, None);
    assert_eq!(record.etag, None);
    assert_eq!(record.etag_strong, None);
    assert_eq!(record.part_path, None);
    assert_eq!(record.part_file_id, None);
    assert_eq!(record.state, DownloadState::Queued);
    assert_eq!(record.checksum_expected.as_deref(), Some(sha.as_str()));
    assert!(db.storage.segments().load(id).await.unwrap().is_empty());
    let missing = downloads.reset_for_restart(new_id(), 9).await;
    assert!(matches!(missing, Err(StorageError::NotFound { .. })));
}
