//! Segment plans, checkpoints, events, settings and write concurrency.

mod common;

use common::{TestDb, new_download, new_id, open_temp, raw_pool};
use forgedm_storage::{NewEvent, ProbeResult, SegmentRecord, SettingKey, StorageError};
use serde_json::json;
use uuid::Uuid;

const URL: &str = "https://example.test/f";

fn seg(seg_no: u32, start: u64, end: Option<u64>, committed: u64) -> SegmentRecord {
    SegmentRecord {
        seg_no,
        start,
        end,
        committed,
        retry_count: 0,
        updated_at_ms: 1,
    }
}

// Test-support helper (clippy.toml allows unwrap in #[test] fns only).
#[allow(clippy::unwrap_used)]
async fn with_size(size: u64) -> (TestDb, Uuid) {
    let db = open_temp().await;
    let id = new_id();
    db.storage
        .downloads()
        .insert(&new_download(id, URL))
        .await
        .unwrap();
    let probe = ProbeResult {
        total_size: Some(size),
        ..ProbeResult::default()
    };
    db.storage
        .downloads()
        .save_probe(id, &probe, 1)
        .await
        .unwrap();
    (db, id)
}

#[tokio::test]
async fn segment_committed_outside_bounds_rejected() {
    let (db, id) = with_size(100).await;
    let segs = db.storage.segments();

    // committed beyond end, and committed before start, are rejected in Rust.
    let err = segs
        .replace_plan(id, &[seg(0, 0, Some(100), 101)])
        .await
        .unwrap_err();
    assert!(
        matches!(err, StorageError::SegmentBounds { seg_no: 0 }),
        "{err:?}"
    );
    let err = segs
        .replace_plan(id, &[seg(0, 10, Some(100), 9)])
        .await
        .unwrap_err();
    assert!(matches!(err, StorageError::SegmentBounds { .. }), "{err:?}");
    assert!(segs.load(id).await.unwrap().is_empty());

    // The schema CHECK constraints are the second line of defence.
    let raw = raw_pool(&db.path).await;
    let inserted = sqlx::query(
        "INSERT INTO segments (download_id, seg_no, start_offset, end_offset, committed_offset, updated_at_ms) \
         VALUES (?1, 0, 0, 100, 101, 1)",
    )
    .bind(id.to_string())
    .execute(&raw)
    .await;
    assert!(inserted.is_err(), "CHECK must reject committed > end");

    // A tiling gap smuggled past the Rust API is caught when loading.
    sqlx::query(
        "INSERT INTO segments (download_id, seg_no, start_offset, end_offset, committed_offset, updated_at_ms) \
         VALUES (?1, 0, 0, 40, 0, 1), (?1, 1, 50, 100, 50, 1)",
    )
    .bind(id.to_string())
    .execute(&raw)
    .await
    .unwrap();
    let err = segs.load(id).await.unwrap_err();
    assert!(matches!(err, StorageError::SegmentTiling { .. }), "{err:?}");
}

#[tokio::test]
async fn replace_plan_and_load_roundtrip() {
    let (db, id) = with_size(100).await;
    let segs = db.storage.segments();
    let plan = [seg(0, 0, Some(50), 10), seg(1, 50, Some(100), 50)];
    segs.replace_plan(id, &plan).await.unwrap();
    assert_eq!(segs.load(id).await.unwrap(), plan);
    let record = db.storage.downloads().get(id).await.unwrap().unwrap();
    assert_eq!(record.bytes_committed, 10, "sum of (committed - start)");

    segs.reset(id, 5).await.unwrap();
    assert!(segs.load(id).await.unwrap().is_empty());
    let record = db.storage.downloads().get(id).await.unwrap().unwrap();
    assert_eq!(record.bytes_committed, 0);
}

#[tokio::test]
async fn segments_cascade_on_delete() {
    let (db, id) = with_size(100).await;
    db.storage
        .segments()
        .replace_plan(id, &[seg(0, 0, Some(100), 0)])
        .await
        .unwrap();
    db.storage
        .events()
        .append(id, &NewEvent::new(1, "note"))
        .await
        .unwrap();

    assert!(db.storage.downloads().delete(id).await.unwrap());
    let raw = raw_pool(&db.path).await;
    let (segments,): (i64,) = sqlx::query_as("SELECT count(*) FROM segments")
        .fetch_one(&raw)
        .await
        .unwrap();
    let (events,): (i64,) = sqlx::query_as("SELECT count(*) FROM download_events")
        .fetch_one(&raw)
        .await
        .unwrap();
    assert_eq!((segments, events), (0, 0));
}

#[tokio::test]
async fn checkpoint_batch_is_atomic_on_mid_failure() {
    let (db, id) = with_size(100).await;
    let segs = db.storage.segments();
    let original = [seg(0, 0, Some(50), 20), seg(1, 50, Some(100), 60)];
    segs.replace_plan(id, &original).await.unwrap();

    // The second row repeats seg_no 0: the PRIMARY KEY fails after the first insert ran,
    // so the delete and first insert must roll back with it.
    let broken = [seg(0, 0, Some(50), 30), seg(0, 50, Some(100), 70)];
    let err = segs.save_checkpoint(id, 50, &broken).await.unwrap_err();
    assert!(matches!(err, StorageError::Database(_)), "{err:?}");

    assert_eq!(
        segs.load(id).await.unwrap(),
        original,
        "old segments intact"
    );
    let record = db.storage.downloads().get(id).await.unwrap().unwrap();
    assert_eq!(
        record.bytes_committed, 30,
        "bytes_committed unchanged (20 + 10)"
    );

    // A good checkpoint afterwards goes through.
    let good = [seg(0, 0, Some(50), 50), seg(1, 50, Some(100), 70)];
    segs.save_checkpoint(id, 70, &good).await.unwrap();
    assert_eq!(segs.load(id).await.unwrap(), good);
    let record = db.storage.downloads().get(id).await.unwrap().unwrap();
    assert_eq!(record.bytes_committed, 70);
}

#[tokio::test]
async fn event_retention_cap() {
    let (db, id) = with_size(100).await;
    for n in 0..250_i64 {
        db.storage
            .events()
            .append(id, &NewEvent::new(n, "tick"))
            .await
            .unwrap();
    }
    let events = db.storage.events().list(id, 500).await.unwrap();
    assert_eq!(events.len(), 200);
    assert_eq!(events[0].at_ms, 249, "newest first");
    assert_eq!(events[199].at_ms, 50, "oldest 50 pruned");

    // The checkpoint transaction prunes too, even if rows were added by another path.
    let raw = raw_pool(&db.path).await;
    for n in 0..30_i64 {
        sqlx::query(
            "INSERT INTO download_events (download_id, at_ms, kind) VALUES (?1, ?2, 'raw')",
        )
        .bind(id.to_string())
        .bind(1000 + n)
        .execute(&raw)
        .await
        .unwrap();
    }
    db.storage
        .segments()
        .save_checkpoint(id, 0, &[seg(0, 0, Some(100), 0)])
        .await
        .unwrap();
    assert_eq!(db.storage.events().list(id, 500).await.unwrap().len(), 200);

    // Oversized detail is a typed error, never a silent truncation.
    let mut big = NewEvent::new(1, "big");
    big.detail_json = Some(format!("\"{}\"", "x".repeat(5000)));
    let err = db.storage.events().append(id, &big).await.unwrap_err();
    assert!(matches!(err, StorageError::ValueTooLarge { .. }), "{err:?}");
}

#[tokio::test]
async fn settings_roundtrip_and_bounds() {
    let db = open_temp().await;
    let settings = db.storage.settings();
    assert!(
        settings
            .get(SettingKey::MaxActiveDownloads)
            .await
            .unwrap()
            .is_none()
    );
    settings
        .put(SettingKey::MaxActiveDownloads, &json!(3), 1)
        .await
        .unwrap();
    settings
        .put(SettingKey::MaxActiveDownloads, &json!(4), 2)
        .await
        .unwrap();
    assert_eq!(
        settings.get(SettingKey::MaxActiveDownloads).await.unwrap(),
        Some(json!(4))
    );

    let huge = json!("y".repeat(16_400));
    let err = settings
        .put(SettingKey::GlobalRateLimitBps, &huge, 3)
        .await
        .unwrap_err();
    assert!(matches!(err, StorageError::ValueTooLarge { .. }), "{err:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_immediate_writers_never_busy() {
    let (db, id) = with_size(100).await;
    let mut tasks = tokio::task::JoinSet::new();
    for worker in 0..8_i64 {
        let storage = db.storage.clone();
        tasks.spawn(async move {
            for n in 0..100_i64 {
                storage
                    .events()
                    .append(id, &NewEvent::new(worker * 1000 + n, "w"))
                    .await?;
                storage
                    .settings()
                    .put(SettingKey::MaxActiveDownloads, &json!(n), n)
                    .await?;
            }
            Ok::<(), StorageError>(())
        });
    }
    while let Some(joined) = tasks.join_next().await {
        joined.unwrap().unwrap();
    }
    // 800 appends capped at the retention limit; readers still work.
    assert_eq!(db.storage.events().list(id, 500).await.unwrap().len(), 200);
}

#[tokio::test]
async fn checkpoint_rejects_bytes_committed_mismatch() {
    let (db, id) = with_size(100).await;
    let segs = db.storage.segments();
    let original = [seg(0, 0, Some(50), 20), seg(1, 50, Some(100), 60)];
    segs.replace_plan(id, &original).await.unwrap();

    // Sum of (committed - start) is 30 + 0; claiming 99 (or 0) must be refused.
    let plan = [seg(0, 0, Some(50), 30), seg(1, 50, Some(100), 50)];
    for claimed in [99_u64, 0, 31] {
        let err = segs.save_checkpoint(id, claimed, &plan).await.unwrap_err();
        assert!(
            matches!(
                err,
                StorageError::InvalidValue {
                    field: "bytes_committed"
                }
            ),
            "{claimed}: {err:?}"
        );
    }
    assert_eq!(segs.load(id).await.unwrap(), original, "nothing changed");
    let record = db.storage.downloads().get(id).await.unwrap().unwrap();
    assert_eq!(record.bytes_committed, 30);

    segs.save_checkpoint(id, 30, &plan).await.unwrap();
    assert_eq!(segs.load(id).await.unwrap(), plan);
}

#[tokio::test]
async fn event_append_for_unknown_download_is_not_found() {
    let db = open_temp().await;
    let ghost = new_id();
    let err = db
        .storage
        .events()
        .append(ghost, &NewEvent::new(1, "note"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, StorageError::NotFound { id } if id == ghost),
        "{err:?}"
    );
}
