//! Ports the engine depends on: persistence ([`DownloadStore`]) and outgoing events
//! ([`EngineEventSink`]). The agent implements both (`store_adapter`, `engine_host`); core
//! tests use the in-memory fakes in `testing`.
//!
//! Core never depends on `forgedm-storage`, so these types are plain data.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use forgedm_protocol::{ClientKind, DownloadError, DownloadState, Priority, TransferMode};
use uuid::Uuid;

use crate::clock::BoxFuture;
use crate::error::StoreError;
use crate::url_policy::SecretUrl;

/// Result of a store call.
pub type StoreFuture<'a, T> = BoxFuture<'a, Result<T, StoreError>>;

/// Wall-clock milliseconds since the Unix epoch (0 before it).
#[must_use]
pub fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

/// A download to create. It always starts `queued`.
#[derive(Clone)]
pub struct NewJob {
    /// UUIDv7 id.
    pub id: Uuid,
    /// Client kind that created it.
    pub source: ClientKind,
    /// Scheduling priority.
    pub priority: Priority,
    /// The validated URL; the store seals it.
    pub url: SecretUrl,
    /// Canonical destination directory (already validated by the caller).
    pub target_dir: PathBuf,
    /// File name the user asked for (sanitised).
    pub requested_name: Option<String>,
    /// Name shown in lists.
    pub display_name: String,
    /// Expected SHA-256, lowercase hex.
    pub expected_sha256: Option<String>,
    /// Per-download connection cap, `1..=16`.
    pub max_connections: u32,
    /// Creation time, epoch milliseconds.
    pub created_at_ms: i64,
}

impl std::fmt::Debug for NewJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewJob")
            .field("id", &self.id)
            .field("url", &self.url.redacted())
            .finish_non_exhaustive()
    }
}

/// What a download had durably achieved when it was loaded: the remote identity learned by
/// the probe and the bytes committed to disk. All defaults mean "nothing yet".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoredProgress {
    /// Complete length, when the probe learned it.
    pub total: Option<u64>,
    /// Bytes durably on disk and in the checkpoint.
    pub committed: u64,
    /// `ETag` as received.
    pub etag: Option<String>,
    /// Whether the `ETag` is strong.
    pub etag_strong: Option<bool>,
    /// `Last-Modified` as received.
    pub last_modified: Option<String>,
    /// Whether `Last-Modified` is strong.
    pub last_modified_strong: Option<bool>,
}

/// A stored download as the task needs it, with the URL already unsealed.
#[derive(Clone)]
pub struct StoredJob {
    /// Download id.
    pub id: Uuid,
    /// Persisted state.
    pub state: DownloadState,
    /// Open-set state reason.
    pub state_reason: Option<String>,
    /// The original URL.
    pub url: SecretUrl,
    /// Canonical destination directory.
    pub target_dir: PathBuf,
    /// Requested file name.
    pub requested_name: Option<String>,
    /// Display name.
    pub display_name: String,
    /// Resolved file name, once probed.
    pub file_name: Option<String>,
    /// Expected SHA-256, lowercase hex.
    pub expected_sha256: Option<String>,
    /// Recorded `.part` path.
    pub part_path: Option<PathBuf>,
    /// Recorded `.part` file id (`volume serial` LE ++ 16 id bytes).
    pub part_file_id: Option<Vec<u8>>,
    /// Transfer mode, once known.
    pub transfer_mode: Option<TransferMode>,
    /// Identity and committed bytes from earlier runs.
    pub progress: StoredProgress,
}

impl std::fmt::Debug for StoredJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredJob")
            .field("id", &self.id)
            .field("state", &self.state)
            .field("url", &self.url.redacted())
            .finish_non_exhaustive()
    }
}

/// Facts learned from the first response, persisted before the first byte is written.
#[derive(Clone)]
pub struct ProbeFacts {
    /// How the body is transferred.
    pub transfer_mode: TransferMode,
    /// Complete length, when known.
    pub total: Option<u64>,
    /// MIME type.
    pub mime: Option<String>,
    /// URL after redirects.
    pub final_url: SecretUrl,
    /// File name chosen.
    pub file_name: String,
    /// Staging file path.
    pub part_path: PathBuf,
    /// Staging file id.
    pub part_file_id: Option<Vec<u8>>,
    /// `ETag` as received.
    pub etag: Option<String>,
    /// Whether the `ETag` is strong.
    pub etag_strong: Option<bool>,
    /// `Last-Modified` as received.
    pub last_modified: Option<String>,
    /// Whether `Last-Modified` is strong.
    pub last_modified_strong: Option<bool>,
}

/// A `download_events` row to record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreEvent {
    /// Event time, epoch milliseconds.
    pub at_ms: i64,
    /// Short machine kind.
    pub kind: String,
    /// Optional error or reason code.
    pub code: Option<String>,
    /// Optional JSON detail (already redacted, at most 4 KiB).
    pub detail: Option<String>,
}

impl StoreEvent {
    /// An event of `kind` now, with no code or detail.
    #[must_use]
    pub fn now(kind: &str) -> Self {
        Self {
            at_ms: unix_ms(),
            kind: kind.to_owned(),
            code: None,
            detail: None,
        }
    }

    /// Adds a code.
    #[must_use]
    pub fn with_code(mut self, code: &str) -> Self {
        self.code = Some(code.to_owned());
        self
    }

    /// Adds a JSON detail.
    #[must_use]
    pub fn with_detail(mut self, detail: String) -> Self {
        self.detail = Some(detail);
        self
    }
}

/// Persistence the engine needs. Every state change is a compare-and-set on `from`
/// ([`StoreError::Conflict`] when it lost) and is durable when the call returns.
pub trait DownloadStore: Send + Sync + 'static {
    /// Inserts a `queued` download.
    fn insert(&self, new: NewJob) -> StoreFuture<'_, ()>;
    /// Loads a download with its URL unsealed.
    fn load(&self, id: Uuid) -> StoreFuture<'_, Option<StoredJob>>;
    /// Downloads to resume at agent start: `queued` and `interrupted`, oldest first.
    fn startup_work(&self) -> StoreFuture<'_, Vec<StoredJob>>;
    /// Number of stored downloads (the queue cap counts them).
    fn count(&self) -> StoreFuture<'_, usize>;
    /// Compare-and-set the state with its event.
    fn transition(
        &self,
        id: Uuid,
        from: DownloadState,
        to: DownloadState,
        reason: Option<String>,
        event: StoreEvent,
    ) -> StoreFuture<'_, ()>;
    /// Persists the first response's facts and the staging file.
    fn save_probe(&self, id: Uuid, facts: ProbeFacts) -> StoreFuture<'_, ()>;
    /// Persists committed progress in one commit: one segment `[0, total)` (or open-ended)
    /// with `committed` bytes durable on disk. Callers flush the file to disk first.
    fn checkpoint(&self, id: Uuid, committed: u64, total: Option<u64>) -> StoreFuture<'_, ()>;
    /// Forgets all progress and the remote identity (validators, total, staging file) in one
    /// transaction: restart from zero.
    fn reset_progress(&self, id: Uuid) -> StoreFuture<'_, ()>;
    /// Deletes the download (its segments and events go with it).
    fn remove(&self, id: Uuid) -> StoreFuture<'_, ()>;
    /// Persists the rename candidate about to be tried (durable before the attempt).
    fn set_finalize_target(&self, id: Uuid, target: &Path) -> StoreFuture<'_, ()>;
    /// Moves `finalizing` to `completed` and records the result.
    fn complete(
        &self,
        id: Uuid,
        final_path: &Path,
        sha256: &str,
        length_verified: bool,
    ) -> StoreFuture<'_, ()>;
    /// Appends an event that is not a transition.
    fn append_event(&self, id: Uuid, event: StoreEvent) -> StoreFuture<'_, ()>;
}

/// Live numbers for one download in a `Progress` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressSample {
    /// Download id.
    pub id: Uuid,
    /// Bytes received and handed to the writer.
    pub accepted: u64,
    /// Bytes durable on disk and in the checkpoint.
    pub committed: u64,
    /// Total size, when known.
    pub total: Option<u64>,
    /// Smoothed speed in bytes per second.
    pub speed_bps: Option<u64>,
    /// Estimated time remaining in milliseconds.
    pub eta_ms: Option<u64>,
    /// Open connections.
    pub active_connections: u32,
}

/// What the engine tells the outside world. Every event follows the change it describes
/// being persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineEvent {
    /// A download was inserted.
    Added {
        /// Download id.
        id: Uuid,
    },
    /// A persisted change (state, probe facts).
    Changed {
        /// Download id.
        id: Uuid,
    },
    /// Coalesced progress of every live download (4 Hz at most).
    Progress(Vec<ProgressSample>),
    /// Emitted after the `Changed` that recorded `completed`.
    Completed {
        /// Download id.
        id: Uuid,
        /// Final path.
        final_path: PathBuf,
        /// SHA-256, lowercase hex.
        sha256: String,
        /// Whether the length was proven.
        length_verified: bool,
    },
    /// Emitted after the `Changed` that recorded `failed`.
    Failed {
        /// Download id.
        id: Uuid,
        /// Why.
        error: DownloadError,
    },
    /// The download was deleted (after the delete committed).
    Removed {
        /// Download id.
        id: Uuid,
    },
}

/// Receives [`EngineEvent`]s. State events wait for room; progress may be dropped.
pub trait EngineEventSink: Send + Sync + 'static {
    /// Delivers `event`, waiting for room.
    fn emit(&self, event: EngineEvent) -> BoxFuture<'_, ()>;
    /// Delivers `event` if there is room right now; returns whether it was queued.
    fn try_emit(&self, event: EngineEvent) -> bool;
}
