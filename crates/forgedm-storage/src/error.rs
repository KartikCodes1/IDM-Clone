//! Storage error type. Messages never carry URLs or sealed values.

use forgedm_protocol::DownloadState;
use uuid::Uuid;

use crate::sealer::SealError;

/// SQLite primary result codes that matter for classification.
const SQLITE_BUSY: i32 = 5;
const SQLITE_LOCKED: i32 = 6;
const SQLITE_CORRUPT: i32 = 11;
const SQLITE_NOTADB: i32 = 26;

/// Everything the storage layer can fail with.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// The file is not a SQLite database or is malformed. The file is never modified or
    /// deleted by this crate (the agent exits 5).
    #[error("database file is corrupt or not a database")]
    Corrupt(#[source] sqlx::Error),
    /// SQLite reported BUSY/LOCKED after the busy timeout.
    #[error("database is busy")]
    Busy(#[source] sqlx::Error),
    /// Any other database failure.
    #[error("database error")]
    Database(#[source] sqlx::Error),
    /// Applying or validating migrations failed (including a database newer than this build).
    #[error("migration failed")]
    Migration(#[from] sqlx::migrate::MigrateError),
    /// The bundled SQLite is older than STRICT tables require.
    #[error("sqlite {found} is older than required {required}")]
    SqliteTooOld {
        /// Version reported by `sqlite_version()`.
        found: String,
        /// Minimum supported version.
        required: &'static str,
    },
    /// The compare-and-set on the download state lost: another writer changed it first.
    #[error("download {id} is not in state {expected:?} (actual: {actual:?})")]
    Conflict {
        /// Download id.
        id: Uuid,
        /// State the caller expected.
        expected: DownloadState,
        /// Current state, when readable.
        actual: Option<DownloadState>,
    },
    /// No row with this id.
    #[error("download {id} not found")]
    NotFound {
        /// Download id.
        id: Uuid,
    },
    /// Sealing or unsealing a secret failed; secret-bearing columns are unavailable.
    #[error("secret sealing failed")]
    Crypto(#[from] SealError),
    /// A stored enum column holds a value this build does not know.
    #[error("unknown value {value:?} in column {column}")]
    UnknownEnumValue {
        /// Column name.
        column: &'static str,
        /// The stored string.
        value: String,
    },
    /// A segment violates `start <= committed <= end` (or `start <= end`).
    #[error("segment {seg_no} is outside its bounds")]
    SegmentBounds {
        /// Segment number.
        seg_no: u32,
    },
    /// The stored segments do not tile `[0, total_size)`.
    #[error("segments do not tile the file: {reason}")]
    SegmentTiling {
        /// What is wrong.
        reason: &'static str,
    },
    /// A value exceeds its documented size cap.
    #[error("{what} exceeds {max} bytes")]
    ValueTooLarge {
        /// Which value.
        what: &'static str,
        /// The cap in bytes.
        max: usize,
    },
    /// A caller-supplied or stored value is not acceptable (range, unknown enum, bad id).
    #[error("invalid value for {field}")]
    InvalidValue {
        /// Field name.
        field: &'static str,
    },
}

impl StorageError {
    /// True when the database file is corrupt or not a database.
    #[must_use]
    pub fn is_corrupt(&self) -> bool {
        matches!(self, Self::Corrupt(_))
    }

    /// True when a secret could not be unsealed (maps to `secrets_unavailable`).
    #[must_use]
    pub fn is_secrets_unavailable(&self) -> bool {
        matches!(self, Self::Crypto(_))
    }
}

impl From<sqlx::Error> for StorageError {
    fn from(err: sqlx::Error) -> Self {
        let code = match &err {
            sqlx::Error::Database(db) => db.code().and_then(|c| c.parse::<i32>().ok()),
            _ => None,
        };
        // Extended result codes keep the primary code in the low byte.
        match code.map(|c| c & 0xff) {
            Some(SQLITE_CORRUPT | SQLITE_NOTADB) => Self::Corrupt(err),
            Some(SQLITE_BUSY | SQLITE_LOCKED) => Self::Busy(err),
            _ => Self::Database(err),
        }
    }
}
