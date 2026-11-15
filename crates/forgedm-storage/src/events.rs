//! `download_events`: append with per-download retention.

use forgedm_protocol::DownloadState;
use sqlx::SqliteConnection;
use uuid::Uuid;

use crate::convert::parse_enum_opt;
use crate::db::Storage;
use crate::error::StorageError;
use crate::models::{EventRecord, NewEvent};
use crate::sealer::id_text;
use crate::{EVENT_DETAIL_MAX_BYTES, EVENT_RETENTION};

/// Rows per `list` call are capped so a caller cannot request an unbounded read.
const EVENT_LIST_MAX: u32 = 500;

type EventRow = (
    i64,
    i64,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// Event repository, from [`Storage::events`].
#[derive(Debug, Clone)]
pub struct Events {
    storage: Storage,
}

/// Reject an oversized or non-JSON detail before it reaches the database.
fn validate_event(event: &NewEvent) -> Result<(), StorageError> {
    let Some(detail) = event.detail_json.as_deref() else {
        return Ok(());
    };
    if detail.len() > EVENT_DETAIL_MAX_BYTES {
        return Err(StorageError::ValueTooLarge {
            what: "detail_json",
            max: EVENT_DETAIL_MAX_BYTES,
        });
    }
    serde_json::from_str::<serde::de::IgnoredAny>(detail)
        .map(|_| ())
        .map_err(|_| StorageError::InvalidValue {
            field: "detail_json",
        })
}

/// Insert one event inside the caller's transaction, then prune to the retention cap.
pub(crate) async fn insert_event(
    conn: &mut SqliteConnection,
    download_id: &str,
    event: &NewEvent,
    from: Option<DownloadState>,
    to: Option<DownloadState>,
) -> Result<(), StorageError> {
    validate_event(event)?;
    sqlx::query(
        "INSERT INTO download_events (download_id, at_ms, kind, from_state, to_state, code, detail_json) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )
    .bind(download_id)
    .bind(event.at_ms)
    .bind(&event.kind)
    .bind(from.map(DownloadState::as_str))
    .bind(to.map(DownloadState::as_str))
    .bind(event.code.as_deref())
    .bind(event.detail_json.as_deref())
    .execute(&mut *conn)
    .await?;
    prune_events(conn, download_id).await
}

/// Keep only the newest [`EVENT_RETENTION`] events of a download.
pub(crate) async fn prune_events(
    conn: &mut SqliteConnection,
    download_id: &str,
) -> Result<(), StorageError> {
    let keep = i64::try_from(EVENT_RETENTION).unwrap_or(i64::MAX);
    sqlx::query(
        "DELETE FROM download_events WHERE download_id = ?1 AND id NOT IN \
         (SELECT id FROM download_events WHERE download_id = ?1 ORDER BY id DESC LIMIT ?2)",
    )
    .bind(download_id)
    .bind(keep)
    .execute(conn)
    .await?;
    Ok(())
}

/// SQLite extended result code `SQLITE_CONSTRAINT_FOREIGNKEY`.
const SQLITE_CONSTRAINT_FOREIGNKEY: &str = "787";

/// A foreign-key failure on an event insert means the download does not exist.
fn missing_download(err: StorageError, id: Uuid) -> StorageError {
    match &err {
        StorageError::Database(sqlx::Error::Database(db))
            if db.code().as_deref() == Some(SQLITE_CONSTRAINT_FOREIGNKEY) =>
        {
            StorageError::NotFound { id }
        }
        _ => err,
    }
}

impl Events {
    pub(crate) fn new(storage: Storage) -> Self {
        Self { storage }
    }

    /// Append a non-transition event (and prune to 200 per download).
    ///
    /// # Errors
    /// [`StorageError::ValueTooLarge`] / [`StorageError::InvalidValue`] for a bad detail,
    /// [`StorageError::NotFound`] for an unknown download id, or a database error.
    pub async fn append(&self, id: Uuid, event: &NewEvent) -> Result<(), StorageError> {
        let mut tx = self.storage.begin_write().await?;
        insert_event(&mut tx, &id_text(id), event, None, None)
            .await
            .map_err(|err| missing_download(err, id))?;
        tx.commit().await?;
        Ok(())
    }

    /// The newest events of a download, newest first, at most 500.
    ///
    /// # Errors
    /// [`StorageError`] on a database failure or an unknown stored state.
    pub async fn list(&self, id: Uuid, limit: u32) -> Result<Vec<EventRecord>, StorageError> {
        let rows: Vec<EventRow> = sqlx::query_as(
            "SELECT id, at_ms, kind, from_state, to_state, code, detail_json \
             FROM download_events WHERE download_id = ?1 ORDER BY id DESC LIMIT ?2",
        )
        .bind(id_text(id))
        .bind(i64::from(limit.min(EVENT_LIST_MAX)))
        .fetch_all(self.storage.reader())
        .await?;
        rows.into_iter().map(event_from_row).collect()
    }
}

fn event_from_row(row: EventRow) -> Result<EventRecord, StorageError> {
    let (id, at_ms, kind, from, to, code, detail_json) = row;
    let states = DownloadState::ALL;
    Ok(EventRecord {
        id,
        at_ms,
        kind,
        from_state: parse_enum_opt("from_state", from.as_deref(), states, DownloadState::as_str)?,
        to_state: parse_enum_opt("to_state", to.as_deref(), states, DownloadState::as_str)?,
        code,
        detail_json,
    })
}
