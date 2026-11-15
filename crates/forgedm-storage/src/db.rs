//! The [`Storage`] handle: pools, pragmas, version check and migrations.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Sqlite, SqlitePool, Transaction};

use crate::downloads::Downloads;
use crate::error::StorageError;
use crate::events::Events;
use crate::sealer::SecretSealer;
use crate::segments::Segments;
use crate::settings::Settings;

/// STRICT tables need SQLite 3.37.
const MIN_SQLITE: (u32, u32, u32) = (3, 37, 0);
const MIN_SQLITE_TEXT: &str = "3.37.0";
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
/// 64 MiB (Appendix A.9).
const JOURNAL_SIZE_LIMIT: &str = "67108864";
const WRITER_CONNECTIONS: u32 = 1;
const READER_CONNECTIONS: u32 = 4;

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// Whether a `sqlite_version()` string such as `3.50.4` is at least `min`. Unparseable
/// strings are not.
#[must_use]
pub fn sqlite_version_at_least(version: &str, min: (u32, u32, u32)) -> bool {
    let mut parts = version.split('.').map(|p| p.parse::<u32>().ok());
    match (
        parts.next().flatten(),
        parts.next().flatten(),
        parts.next().flatten(),
    ) {
        (Some(major), Some(minor), Some(patch)) => (major, minor, patch) >= min,
        _ => false,
    }
}

/// Effective connection pragmas, for diagnostics and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PragmaReport {
    /// Writer `journal_mode`.
    pub journal_mode: String,
    /// Writer `synchronous` (2 is FULL).
    pub synchronous: i64,
    /// Writer `foreign_keys`.
    pub foreign_keys: i64,
    /// Writer `busy_timeout` in ms.
    pub busy_timeout_ms: i64,
    /// Writer `journal_size_limit` in bytes.
    pub journal_size_limit: i64,
    /// Reader `query_only`.
    pub reader_query_only: i64,
    /// Reader `synchronous`.
    pub reader_synchronous: i64,
}

struct Inner {
    writer: SqlitePool,
    readers: SqlitePool,
    sealer: Arc<dyn SecretSealer>,
    sqlite_version: String,
}

/// The database handle. Cheap to clone; all clones share the pools.
#[derive(Clone)]
pub struct Storage {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Storage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Storage")
            .field("sqlite_version", &self.inner.sqlite_version)
            .finish()
    }
}

impl Storage {
    /// Open (creating if missing) and migrate the database at `path`.
    ///
    /// Only the agent calls this: SQLite migration locks are no-ops, so a second migrating
    /// process is not supported. A corrupt file is reported as [`StorageError::Corrupt`]
    /// and left untouched.
    ///
    /// # Errors
    /// [`StorageError`] when the file cannot be opened or migrated, or SQLite is too old.
    pub async fn open(
        path: impl AsRef<Path>,
        sealer: Arc<dyn SecretSealer>,
    ) -> Result<Self, StorageError> {
        let writer_options = SqliteConnectOptions::new()
            .filename(path.as_ref())
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .foreign_keys(true)
            .busy_timeout(BUSY_TIMEOUT)
            .pragma("journal_size_limit", JOURNAL_SIZE_LIMIT);
        let writer = SqlitePoolOptions::new()
            .max_connections(WRITER_CONNECTIONS)
            .connect_with(writer_options.clone())
            .await?;

        let sqlite_version: String = sqlx::query_scalar("SELECT sqlite_version()")
            .fetch_one(&writer)
            .await?;
        if !sqlite_version_at_least(&sqlite_version, MIN_SQLITE) {
            return Err(StorageError::SqliteTooOld {
                found: sqlite_version,
                required: MIN_SQLITE_TEXT,
            });
        }
        MIGRATOR.run(&writer).await?;

        let reader_options = writer_options
            .create_if_missing(false)
            .pragma("query_only", "ON");
        let readers = SqlitePoolOptions::new()
            .max_connections(READER_CONNECTIONS)
            .connect_with(reader_options)
            .await?;
        Ok(Self {
            inner: Arc::new(Inner {
                writer,
                readers,
                sealer,
                sqlite_version,
            }),
        })
    }

    /// Close both pools. The last connection closing checkpoints and removes the WAL.
    pub async fn close(self) {
        self.inner.readers.close().await;
        self.inner.writer.close().await;
    }

    /// The runtime `sqlite_version()`.
    #[must_use]
    pub fn sqlite_version(&self) -> &str {
        &self.inner.sqlite_version
    }

    /// Highest applied migration version.
    ///
    /// # Errors
    /// [`StorageError`] on a database failure.
    pub async fn schema_version(&self) -> Result<i64, StorageError> {
        let version = sqlx::query_scalar("SELECT COALESCE(MAX(version), 0) FROM _sqlx_migrations")
            .fetch_one(&self.inner.readers)
            .await?;
        Ok(version)
    }

    /// Names of all tables, sorted.
    ///
    /// # Errors
    /// [`StorageError`] on a database failure.
    pub async fn table_names(&self) -> Result<Vec<String>, StorageError> {
        let names =
            sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
                .fetch_all(&self.inner.readers)
                .await?;
        Ok(names)
    }

    /// Read back the pragmas of a writer and a reader connection.
    ///
    /// # Errors
    /// [`StorageError`] on a database failure.
    pub async fn pragmas(&self) -> Result<PragmaReport, StorageError> {
        let w = &self.inner.writer;
        let r = &self.inner.readers;
        Ok(PragmaReport {
            journal_mode: sqlx::query_scalar("PRAGMA journal_mode")
                .fetch_one(w)
                .await?,
            synchronous: sqlx::query_scalar("PRAGMA synchronous")
                .fetch_one(w)
                .await?,
            foreign_keys: sqlx::query_scalar("PRAGMA foreign_keys")
                .fetch_one(w)
                .await?,
            busy_timeout_ms: sqlx::query_scalar("PRAGMA busy_timeout")
                .fetch_one(w)
                .await?,
            journal_size_limit: sqlx::query_scalar("PRAGMA journal_size_limit")
                .fetch_one(w)
                .await?,
            reader_query_only: sqlx::query_scalar("PRAGMA query_only").fetch_one(r).await?,
            reader_synchronous: sqlx::query_scalar("PRAGMA synchronous")
                .fetch_one(r)
                .await?,
        })
    }

    /// Download repository.
    #[must_use]
    pub fn downloads(&self) -> Downloads {
        Downloads::new(self.clone())
    }

    /// Segment repository.
    #[must_use]
    pub fn segments(&self) -> Segments {
        Segments::new(self.clone())
    }

    /// Event repository.
    #[must_use]
    pub fn events(&self) -> Events {
        Events::new(self.clone())
    }

    /// Settings repository.
    #[must_use]
    pub fn settings(&self) -> Settings {
        Settings::new(self.clone())
    }

    /// Begin a write transaction. Always `BEGIN IMMEDIATE`, so a write never upgrades a
    /// read lock and never fails with a deferred `SQLITE_BUSY`.
    pub(crate) async fn begin_write(&self) -> Result<Transaction<'static, Sqlite>, StorageError> {
        Ok(self.inner.writer.begin_with("BEGIN IMMEDIATE").await?)
    }

    /// The reader pool (`query_only` connections).
    pub(crate) fn reader(&self) -> &SqlitePool {
        &self.inner.readers
    }

    pub(crate) fn sealer(&self) -> &dyn SecretSealer {
        self.inner.sealer.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sqlx::Connection;

    use super::*;
    use crate::sealer::{SealContext, SealError};

    #[derive(Debug)]
    struct PlainSealer;

    impl SecretSealer for PlainSealer {
        fn seal(&self, plaintext: &[u8], _: &SealContext<'_>) -> Result<Vec<u8>, SealError> {
            Ok(plaintext.to_vec())
        }

        fn unseal(&self, sealed: &[u8], _: &SealContext<'_>) -> Result<Vec<u8>, SealError> {
            Ok(sealed.to_vec())
        }
    }

    /// While another connection holds the write lock, `begin_write` must fail at BEGIN
    /// itself with `Busy` after the busy timeout. A deferred `BEGIN` would return `Ok` here
    /// and only fail on the first write, so this pins the `BEGIN IMMEDIATE` property.
    #[tokio::test]
    async fn begin_write_takes_the_write_lock_up_front() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("forgedm.db");
        let storage = Storage::open(&path, Arc::new(PlainSealer)).await.unwrap();

        let mut holder =
            sqlx::SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&path))
                .await
                .unwrap();
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut holder)
            .await
            .unwrap();

        let result = storage.begin_write().await;
        assert!(
            matches!(result, Err(StorageError::Busy(_))),
            "{:?}",
            result.map(|_| ())
        );

        sqlx::query("ROLLBACK").execute(&mut holder).await.unwrap();
        storage
            .begin_write()
            .await
            .unwrap()
            .rollback()
            .await
            .unwrap();
    }
}
