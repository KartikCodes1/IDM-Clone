//! Canary: sealed URLs must not appear in the database, WAL or SHM bytes.

mod common;

use common::{new_download, new_id, open_temp};
use forgedm_storage::ProbeResult;

const CANARY: &str = "CANARY-8f3a91c7d2e4b650";

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_bytes())
}

#[tokio::test]
async fn sealed_columns_raw_db_and_wal_bytes_lack_canary() {
    let db = open_temp().await;
    let id = new_id();
    let url = format!("https://example.test/path?token={CANARY}#frag-{CANARY}");
    db.storage
        .downloads()
        .insert(&new_download(id, &url))
        .await
        .unwrap();
    let probe = ProbeResult {
        final_url: Some(format!("https://cdn.test/redirected?sig={CANARY}")),
        ..ProbeResult::default()
    };
    db.storage
        .downloads()
        .save_probe(id, &probe, 2)
        .await
        .unwrap();
    // Round trip still works, so the canary was really stored (sealed).
    assert!(
        db.storage
            .downloads()
            .original_url(id)
            .await
            .unwrap()
            .expose()
            .contains(CANARY)
    );

    let mut all = Vec::new();
    for suffix in ["", "-wal", "-shm"] {
        let mut name = db.path.clone().into_os_string();
        name.push(suffix);
        if let Ok(bytes) = std::fs::read(&name) {
            all.push((suffix, bytes));
        }
    }
    let wal = all
        .iter()
        .find(|(s, _)| *s == "-wal")
        .map_or(0, |(_, b)| b.len());
    assert!(
        wal > 0,
        "WAL should hold the committed rows (positive control)"
    );
    // Positive control: a plaintext column is visible in the same bytes.
    assert!(
        all.iter()
            .any(|(_, b)| contains(b, "https://example.test/file.bin"))
    );
    for (suffix, bytes) in &all {
        assert!(!contains(bytes, CANARY), "canary found in db{suffix}");
        assert!(!contains(bytes, "token="), "query found in db{suffix}");
    }

    // After a close (full checkpoint) the main file must be clean too.
    let path = db.path.clone();
    db.storage.close().await;
    let bytes = std::fs::read(&path).unwrap();
    assert!(!contains(&bytes, CANARY));
}
