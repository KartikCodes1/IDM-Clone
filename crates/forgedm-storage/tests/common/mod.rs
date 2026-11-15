//! Shared helpers: a deterministic test sealer that enforces context binding, temp
//! databases, and a raw side-channel pool for tampering with rows.
// clippy.toml allows unwrap in #[test] functions only; these are test-support helpers.
#![allow(dead_code, clippy::unwrap_used)]

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use forgedm_protocol::{ClientKind, Priority};
use forgedm_storage::{NewDownload, SealContext, SealError, SecretSealer, Storage};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tempfile::TempDir;
use uuid::Uuid;

const FORMAT_BYTE: u8 = 0x01;
const TAG_LEN: usize = 8;

fn hash_of(parts: &[&[u8]], counter: u64) -> u64 {
    let mut hasher = DefaultHasher::new();
    for part in parts {
        part.hash(&mut hasher);
    }
    counter.hash(&mut hasher);
    hasher.finish()
}

/// Deterministic stand-in for DPAPI: `0x01 || tag || plaintext xor keystream`, both keyed by
/// the entropy `ForgeDM/v1/<column>/<download_id>`, so a value moved to another row or column
/// fails the tag check, like DPAPI with different entropy.
#[derive(Debug, Default)]
pub struct TestSealer;

impl TestSealer {
    fn keystream_xor(entropy: &[u8], data: &[u8]) -> Vec<u8> {
        data.iter()
            .enumerate()
            .map(|(i, byte)| {
                let word = hash_of(&[entropy], (i / 8) as u64).to_le_bytes();
                byte ^ word[i % 8]
            })
            .collect()
    }
}

impl SecretSealer for TestSealer {
    fn seal(&self, plaintext: &[u8], ctx: &SealContext<'_>) -> Result<Vec<u8>, SealError> {
        let entropy = ctx.entropy();
        let tag = hash_of(&[entropy.as_bytes(), plaintext], 0).to_le_bytes();
        let mut out = vec![FORMAT_BYTE];
        out.extend_from_slice(&tag);
        out.extend(Self::keystream_xor(entropy.as_bytes(), plaintext));
        Ok(out)
    }

    fn unseal(&self, sealed: &[u8], ctx: &SealContext<'_>) -> Result<Vec<u8>, SealError> {
        let entropy = ctx.entropy();
        if sealed.len() < 1 + TAG_LEN || sealed[0] != FORMAT_BYTE {
            return Err(SealError::Unseal("malformed blob".into()));
        }
        let plaintext = Self::keystream_xor(entropy.as_bytes(), &sealed[1 + TAG_LEN..]);
        let tag = hash_of(&[entropy.as_bytes(), &plaintext], 0).to_le_bytes();
        if tag != sealed[1..1 + TAG_LEN] {
            return Err(SealError::Unseal("tag mismatch".into()));
        }
        Ok(plaintext)
    }
}

/// A sealer whose operations always fail.
#[derive(Debug)]
pub struct FailingSealer;

impl SecretSealer for FailingSealer {
    fn seal(&self, _: &[u8], _: &SealContext<'_>) -> Result<Vec<u8>, SealError> {
        Err(SealError::Seal("injected".into()))
    }

    fn unseal(&self, _: &[u8], _: &SealContext<'_>) -> Result<Vec<u8>, SealError> {
        Err(SealError::Unseal("injected".into()))
    }
}

pub struct TestDb {
    pub dir: TempDir,
    pub path: PathBuf,
    pub storage: Storage,
}

pub async fn open_temp() -> TestDb {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("forgedm.db");
    let storage = Storage::open(&path, Arc::new(TestSealer)).await.unwrap();
    TestDb { dir, path, storage }
}

pub async fn raw_pool(path: &Path) -> SqlitePool {
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(path))
        .await
        .unwrap()
}

pub fn new_id() -> Uuid {
    Uuid::now_v7()
}

pub fn new_download(id: Uuid, url: &str) -> NewDownload {
    NewDownload {
        id,
        source: ClientKind::Cli,
        priority: Priority::Normal,
        queue_position: 1024,
        original_url: url.to_owned(),
        url_display: "https://example.test/file.bin".to_owned(),
        target_dir: "C:\\Users\\test\\Downloads".to_owned(),
        requested_name: None,
        display_name: "file.bin".to_owned(),
        collision_policy: "rename".to_owned(),
        max_connections: 8,
        created_at_ms: 1_700_000_000_000,
    }
}
