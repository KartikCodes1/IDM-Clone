//! Offsets beyond 4 GiB (PLAN S1.11): positional writes, the segment table and sparse files
//! must not truncate offsets to 32 bits. The writing test touches a few chunks at far offsets
//! only; the 5 GiB end-to-end comparison is `#[ignore]` and runs at the gate.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code

#[path = "../src/testing.rs"]
#[allow(dead_code)] // each test file uses part of the shared fakes
mod testing;

use std::os::windows::fs::FileExt;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use bytes::Bytes;
use forgedm_core::client::{HttpClient, ProxyMode};
use forgedm_core::engine::{self, EngineConfig, EngineDeps};
use forgedm_core::ports::{EngineEvent, NewJob, unix_ms};
use forgedm_core::transfer::prealloc::{is_sparse, prepare_part_on_volume};
use forgedm_core::transfer::segment_table::SegmentTable;
use forgedm_core::transfer::segmented::TableTracker;
use forgedm_core::url_policy::validate_user_url;
use forgedm_core::writer::Writer;
use forgedm_fixture::{FixtureServer, verify};
use forgedm_platform::fs::{allocated_size, volume_info};
use forgedm_platform::motw::MotwService;
use forgedm_protocol::{ClientKind, Priority};
use testing::{MemoryStore, RecordingSink};
use uuid::Uuid;

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;
const CHUNK: u64 = 64 * 1024;

#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::print_stderr)] // the runtime skip message is the point
async fn worker_writes_beyond_4gib_offsets_sparse() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("far.part");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    if !volume_info(&file).is_ok_and(|v| v.supports_sparse) {
        eprintln!(
            "SKIPPED worker_writes_beyond_4gib_offsets_sparse: no sparse support on the temp volume"
        );
        return;
    }
    let total = 6 * GIB;
    assert!(prepare_part_on_volume(&file, total).unwrap());
    assert!(is_sparse(&file).unwrap());
    let probe = file.try_clone().unwrap();

    // The table plans four 1.5 GiB segments; each worker writes one chunk at its own start,
    // two of which lie beyond 4 GiB.
    let table = Arc::new(std::sync::Mutex::new(SegmentTable::plan(total, 4)));
    let tracker = Arc::new(TableTracker(Arc::clone(&table)));
    let mut writer =
        Writer::spawn_tracked("far".to_owned(), file, Arc::new(AtomicU64::new(0)), tracker)
            .unwrap();
    let handle = writer.handle();
    let chunk = Bytes::from(vec![0xAB_u8; 64 * 1024]);
    let mut offsets = Vec::new();
    for worker in 1..=4_u32 {
        let (seg_no, start) = {
            let mut t = table.lock().unwrap();
            let a = t.assign(worker).unwrap();
            t.claim(a.seg_no, worker, a.claim, CHUNK).unwrap();
            t.mark_sent(a.seg_no, a.claim + CHUNK).unwrap();
            (a.seg_no, a.claim)
        };
        handle.send(start, chunk.clone()).await.unwrap();
        offsets.push((seg_no, start));
    }
    assert!(
        offsets.iter().filter(|(_, s)| *s > 4 * GIB).count() >= 1,
        "{offsets:?}"
    );

    writer.flush().await.unwrap();

    let view = table.lock().unwrap().checkpoint_view();
    assert_eq!(view.records.len(), 4);
    for rec in &view.records {
        assert_eq!(rec.committed, rec.start + CHUNK, "{rec:?}");
    }
    writer.close().await.unwrap();
    let last_start = offsets.last().unwrap().1;
    for (_, start) in &offsets {
        let mut back = vec![0u8; 64 * 1024];
        probe.seek_read(&mut back, *start).unwrap();
        assert!(back.iter().all(|b| *b == 0xAB), "data at {start}");
    }
    assert_eq!(probe.metadata().unwrap().len(), last_start + CHUNK);
    assert!(
        allocated_size(&path).unwrap() < 64 * MIB,
        "four 64 KiB chunks in a sparse file of {} bytes",
        last_start + CHUNK
    );
}

/// Downloads 5 GiB through the segmented engine and compares every byte with the generator.
/// Slow and disk hungry (the file is 5 GiB), so it only runs on request:
/// `cargo test -p forgedm-core --test large_offsets -- --ignored`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "writes 5 GiB; run at the gate"]
async fn full_5gib_segmented_exact_size_stream_compare() {
    let size = 5 * GIB;
    let server = FixtureServer::start_on(0).await.unwrap();
    let store = Arc::new(MemoryStore::new());
    let sink = Arc::new(RecordingSink::new());
    let engine = engine::start(
        EngineConfig::default(),
        EngineDeps {
            store: store.clone(),
            sink: sink.clone(),
            client: HttpClient::new(ProxyMode::None).unwrap(),
            motw: Arc::new(MotwService::start().unwrap()),
        },
    );
    let dir = tempfile::tempdir().unwrap();
    let url = format!("{}/f/61/{size}", server.base_url());
    let id = Uuid::now_v7();
    engine
        .enqueue(NewJob {
            id,
            source: ClientKind::Cli,
            priority: Priority::Normal,
            display_name: "big.bin".to_owned(),
            url: validate_user_url(&url).unwrap(),
            target_dir: std::fs::canonicalize(dir.path()).unwrap(),
            requested_name: Some("big.bin".to_owned()),
            expected_sha256: None,
            max_connections: 8,
            created_at_ms: unix_ms(),
        })
        .await
        .unwrap();

    let event = sink.wait_terminal(id, Duration::from_secs(1800)).await;

    let EngineEvent::Completed { final_path, .. } = event else {
        panic!("{event:?}");
    };
    assert_eq!(std::fs::metadata(&final_path).unwrap().len(), size);
    verify::file_matches(&final_path, 61, size).unwrap();
}
