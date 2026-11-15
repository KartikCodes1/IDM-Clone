//! ForgeDM SQLite storage (ARCHITECTURE section 7, ADR-011).
//!
//! One writer connection (every write `BEGIN IMMEDIATE`) plus four `query_only` reader
//! connections over a WAL database with `synchronous=FULL`. All SQL is `&'static str` with
//! binds. URLs are stored only as blobs sealed through the [`SecretSealer`] port. Enum
//! columns are validated in Rust on load, and segment tiling is validated when segments are
//! loaded.
//!
//! This crate depends only on `forgedm-protocol`; the platform DPAPI sealer is injected by
//! the agent.

#![forbid(unsafe_code)]

mod convert;
mod db;
mod downloads;
mod error;
mod events;
mod models;
mod record;
mod sealer;
mod segments;
mod settings;

pub use db::{PragmaReport, Storage, sqlite_version_at_least};
pub use downloads::Downloads;
pub use error::StorageError;
pub use events::Events;
pub use models::{
    DownloadRecord, EventRecord, LastError, NewDownload, NewEvent, ProbeResult, SegmentRecord,
};
pub use sealer::{SealColumn, SealContext, SealError, SecretSealer, SecretUrl};
pub use segments::Segments;
pub use settings::{SettingKey, Settings};

/// `download_events` rows kept per download (Appendix A.9).
pub const EVENT_RETENTION: usize = 200;
/// Largest `detail_json` in bytes (Appendix A.9).
pub const EVENT_DETAIL_MAX_BYTES: usize = 4096;
/// Largest settings `value_json` in bytes (Appendix A.9).
pub const SETTING_VALUE_MAX_BYTES: usize = 16 * 1024;
