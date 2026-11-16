//! Startup reconcile of downloads left in `finalizing` (ARCHITECTURE 6.3 step 2, PCR-01),
//! on a real database with the real DPAPI sealer and real files.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use forgedm_agent::reconcile::{PartsReport, finalizing_parts};
use forgedm_agent::sealer::DpapiSealer;
use forgedm_core::transfer::encode_file_id;
use forgedm_fixture::{content, verify};
use forgedm_platform::fs::{create_part_file, file_id};
use forgedm_protocol::{ClientKind, DownloadState, Priority, TransferMode};
use forgedm_storage::{
    NewDownload, NewEvent, ProbeResult, SealContext, SealError, SecretSealer, Storage,
};
use tempfile::TempDir;
use uuid::Uuid;

const SEED: u64 = 77;
const SIZE: u64 = 300_000;
const URL: &str = "https://example.com/files/data.bin?token=CANARY_Q";

/// The real DPAPI sealer, which can be told to fail every unseal (the key is gone).
#[derive(Debug, Clone, Default)]
struct FlakySealer {
    fail_unseal: Arc<AtomicBool>,
}

impl SecretSealer for FlakySealer {
    fn seal(&self, plaintext: &[u8], ctx: &SealContext<'_>) -> Result<Vec<u8>, SealError> {
        DpapiSealer.seal(plaintext, ctx)
    }

    fn unseal(&self, sealed: &[u8], ctx: &SealContext<'_>) -> Result<Vec<u8>, SealError> {
        if self.fail_unseal.load(Ordering::SeqCst) {
            return Err(SealError::Unseal("the key is gone".to_owned()));
        }
        DpapiSealer.unseal(sealed, ctx)
    }
}

struct Rig {
    _db_dir: TempDir,
    target: TempDir,
    storage: Storage,
}

impl Rig {
    async fn new() -> Self {
        Self::with_sealer(Arc::new(DpapiSealer)).await
    }

    async fn with_sealer(sealer: Arc<dyn SecretSealer>) -> Self {
        let db_dir = TempDir::new().unwrap();
        let storage = Storage::open(db_dir.path().join("t.db"), sealer)
            .await
            .unwrap();
        Self {
            _db_dir: db_dir,
            target: TempDir::new().unwrap(),
            storage,
        }
    }

    fn dir(&self) -> PathBuf {
        std::fs::canonicalize(self.target.path()).unwrap()
    }

    /// A `.part` of the generator's bytes with a recorded id.
    fn part(&self, name: &str) -> (PathBuf, Vec<u8>) {
        let path = self.dir().join(name);
        let file = create_part_file(&path).unwrap();
        let mut bytes = vec![0u8; usize::try_from(SIZE).unwrap()];
        content::fill(SEED, 0, &mut bytes);
        std::os::windows::fs::FileExt::seek_write(&file, &bytes, 0).unwrap();
        (path, encode_file_id(&file_id(&file).unwrap()))
    }

    /// A download in `finalizing` that recorded `part` and its id.
    async fn finalizing(&self, part: &Path, id: &[u8]) -> Uuid {
        let uuid = Uuid::now_v7();
        let downloads = self.storage.downloads();
        downloads
            .insert(&NewDownload {
                id: uuid,
                source: ClientKind::Cli,
                priority: Priority::Normal,
                queue_position: 0,
                original_url: URL.to_owned(),
                url_display: "https://example.com/files/data.bin".to_owned(),
                target_dir: self.dir().to_string_lossy().into_owned(),
                requested_name: Some("data.bin".to_owned()),
                display_name: "data.bin".to_owned(),
                collision_policy: "rename".to_owned(),
                max_connections: 4,
                created_at_ms: 1,
            })
            .await
            .unwrap();
        let probe = ProbeResult {
            transfer_mode: Some(TransferMode::SingleRanged),
            total_size: Some(SIZE),
            final_url: Some(URL.to_owned()),
            file_name: Some("data.bin".to_owned()),
            part_path: Some(part.to_string_lossy().into_owned()),
            part_file_id: Some(id.to_vec()),
            etag: Some("\"v1\"".to_owned()),
            etag_strong: Some(true),
            ..ProbeResult::default()
        };
        downloads.save_probe(uuid, &probe, 2).await.unwrap();
        let event = NewEvent::new(3, "state");
        let (from, to) = (DownloadState::Queued, DownloadState::Finalizing);
        downloads
            .transition(uuid, from, to, None, &event)
            .await
            .unwrap();
        uuid
    }

    async fn state(&self, id: Uuid) -> DownloadState {
        self.storage
            .downloads()
            .get(id)
            .await
            .unwrap()
            .unwrap()
            .state
    }

    async fn event_codes(&self, id: Uuid) -> Vec<(String, Option<String>)> {
        self.storage
            .events()
            .list(id, 100)
            .await
            .unwrap()
            .into_iter()
            .map(|e| (e.kind, e.code))
            .collect()
    }
}

fn zone_identifier(path: &Path) -> String {
    let stream = format!("{}:Zone.Identifier", path.display());
    std::fs::read_to_string(stream).expect("the file carries a Zone.Identifier stream")
}

#[tokio::test(flavor = "multi_thread")]
async fn reconcile_finalizing_both_branches() {
    let rig = Rig::new().await;

    // Branch 1: the `.part` is still there with its recorded id: finalize is redone.
    let (part, id) = rig.part("one.forgedm.part");
    let redo = rig.finalizing(&part, &id).await;
    // Branch 2: the rename already happened: the file at the recorded candidate has the id.
    let (moved_part, moved_id) = rig.part("two.forgedm.part");
    let moved = rig.finalizing(&moved_part, &moved_id).await;
    let moved_to = rig.dir().join("moved.bin");
    forgedm_platform::fs::move_no_replace(&moved_part, &moved_to).unwrap();
    rig.storage
        .downloads()
        .set_finalize_target(moved, &moved_to.to_string_lossy(), 4)
        .await
        .unwrap();
    // Branch 3: no file with the recorded id anywhere.
    let (gone_part, gone_id) = rig.part("three.forgedm.part");
    let gone = rig.finalizing(&gone_part, &gone_id).await;
    std::fs::remove_file(&gone_part).unwrap();

    let report = finalizing_parts(&rig.storage).await.unwrap();

    assert_eq!(
        report,
        PartsReport {
            redone: 1,
            adopted: 1,
            failed: 1
        }
    );
    let sha = hex::encode(content::sha256(SEED, SIZE));
    for (id, name) in [(redo, "data.bin"), (moved, "moved.bin")] {
        let record = rig.storage.downloads().get(id).await.unwrap().unwrap();
        assert_eq!(record.state, DownloadState::Completed, "{name}");
        let path = PathBuf::from(record.final_path.expect("a final path"));
        assert_eq!(path.file_name().unwrap(), name);
        assert_eq!(record.checksum_actual.as_deref(), Some(sha.as_str()));
        verify::file_matches(&path, SEED, SIZE).unwrap();
        assert!(
            zone_identifier(&path).contains("ZoneId=3"),
            "{name} is marked as downloaded from the internet"
        );
        assert!(!zone_identifier(&path).contains("CANARY_Q"));
    }
    assert!(!part.exists(), "the redone part was renamed");
    assert_eq!(rig.state(gone).await, DownloadState::Failed);
    let events = rig.event_codes(gone).await;
    assert!(
        events.contains(&("failed".to_owned(), Some("part_file_invalid".to_owned()))),
        "{events:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn reconcile_never_adopts_foreign_same_size_file() {
    let rig = Rig::new().await;
    let (part, id) = rig.part("x.forgedm.part");
    let download = rig.finalizing(&part, &id).await;
    std::fs::remove_file(&part).unwrap();
    // Someone else's file: same name, same size, other bytes.
    let foreign = rig.dir().join("data.bin");
    let bytes = vec![0x5A_u8; usize::try_from(SIZE).unwrap()];
    std::fs::write(&foreign, &bytes).unwrap();
    rig.storage
        .downloads()
        .set_finalize_target(download, &foreign.to_string_lossy(), 4)
        .await
        .unwrap();

    let report = finalizing_parts(&rig.storage).await.unwrap();

    assert_eq!(report.failed, 1);
    assert_eq!(report.adopted, 0);
    let record = rig
        .storage
        .downloads()
        .get(download)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.state, DownloadState::Failed);
    assert_eq!(record.final_path, None, "the foreign file was not adopted");
    assert_eq!(std::fs::read(&foreign).unwrap(), bytes, "and not touched");
    assert!(
        std::fs::read_to_string(format!("{}:Zone.Identifier", foreign.display())).is_err(),
        "no Mark-of-the-Web was written onto a file that is not ours"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn reconcile_leaves_other_states_alone_and_is_idempotent() {
    let rig = Rig::new().await;
    let (part, id) = rig.part("y.forgedm.part");
    let download = rig.finalizing(&part, &id).await;

    let first = finalizing_parts(&rig.storage).await.unwrap();
    let second = finalizing_parts(&rig.storage).await.unwrap();

    assert_eq!(first.redone, 1);
    assert_eq!(
        second,
        PartsReport::default(),
        "nothing is left in finalizing"
    );
    assert_eq!(rig.state(download).await, DownloadState::Completed);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unsealable_url_fails_that_download_and_not_the_agent_start() {
    let sealer = FlakySealer::default();
    let rig = Rig::with_sealer(Arc::new(sealer.clone())).await;
    let (part, id) = rig.part("s.forgedm.part");
    let download = rig.finalizing(&part, &id).await;
    sealer.fail_unseal.store(true, Ordering::SeqCst);

    let report = finalizing_parts(&rig.storage)
        .await
        .expect("one unreadable row does not block startup");

    assert_eq!(report.failed, 1);
    assert_eq!(rig.state(download).await, DownloadState::Failed);
    let events = rig.event_codes(download).await;
    assert!(
        events.contains(&("failed".to_owned(), Some("secrets_unavailable".to_owned()))),
        "{events:?}"
    );
    assert!(part.exists(), "the intact .part is kept");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_locked_part_fails_as_finalize_locked_and_keeps_the_file() {
    use std::os::windows::fs::OpenOptionsExt;
    let rig = Rig::new().await;
    let (part, id) = rig.part("l.forgedm.part");
    let download = rig.finalizing(&part, &id).await;
    // Hold the file without sharing, as an antivirus scan would.
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&part)
        .unwrap();

    let report = finalizing_parts(&rig.storage).await.unwrap();
    drop(held);

    assert_eq!(report.failed, 1);
    let events = rig.event_codes(download).await;
    assert!(
        events.contains(&("failed".to_owned(), Some("finalize_locked".to_owned()))),
        "a lock is not a missing file: {events:?}"
    );
    assert!(part.exists());
}
