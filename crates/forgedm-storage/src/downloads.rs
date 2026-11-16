//! Download repository. Every write is a `BEGIN IMMEDIATE` transaction; state changes are
//! compare-and-set on the previous state.

use forgedm_protocol::{DownloadState, LIST_PAGE_MAX};
use uuid::Uuid;

use crate::convert::to_i64;
use crate::db::Storage;
use crate::error::StorageError;
use crate::events::insert_event;
use crate::models::{DownloadRecord, NewDownload, NewEvent, ProbeResult};
use crate::record::{RawDownload, download_columns, parse_state, priority_to_db};
use crate::sealer::{SealColumn, SealContext, SecretUrl, id_text};

const MIN_CONNECTIONS: u32 = 1;
const MAX_CONNECTIONS: u32 = 16;

/// Download repository, from [`Storage::downloads`].
#[derive(Debug, Clone)]
pub struct Downloads {
    storage: Storage,
}

/// Whether `text` is 64 lowercase hex characters.
fn is_lower_hex_sha256(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn reject_unknown(state: DownloadState) -> Result<(), StorageError> {
    if state == DownloadState::Unknown {
        return Err(StorageError::InvalidValue { field: "state" });
    }
    Ok(())
}

impl Downloads {
    pub(crate) fn new(storage: Storage) -> Self {
        Self { storage }
    }

    fn seal(&self, column: SealColumn, id: &str, url: &str) -> Result<Vec<u8>, StorageError> {
        let ctx = SealContext {
            column,
            download_id: id,
        };
        Ok(self.storage.sealer().seal(url.as_bytes(), &ctx)?)
    }

    fn unseal(&self, column: SealColumn, id: &str, blob: &[u8]) -> Result<SecretUrl, StorageError> {
        let ctx = SealContext {
            column,
            download_id: id,
        };
        let plain = self.storage.sealer().unseal(blob, &ctx)?;
        let text = String::from_utf8(plain).map_err(|_| StorageError::InvalidValue {
            field: "sealed url",
        })?;
        Ok(SecretUrl::new(text))
    }

    /// Insert a new `queued` download. The URL is sealed first; a seal failure inserts
    /// nothing.
    ///
    /// # Errors
    /// [`StorageError::Crypto`], [`StorageError::InvalidValue`] or a database error.
    pub async fn insert(&self, new: &NewDownload) -> Result<(), StorageError> {
        self.insert_with_checksum(new, None).await
    }

    /// [`insert`](Self::insert) that also records the expected SHA-256 (64 lowercase hex
    /// characters) in the `checksum_expected` column, in the same transaction, so the
    /// expectation can never be lost or pruned.
    ///
    /// # Errors
    /// [`StorageError::InvalidValue`] when `expected_sha256` is not 64 lowercase hex
    /// characters, plus the errors of [`insert`](Self::insert).
    pub async fn insert_with_checksum(
        &self,
        new: &NewDownload,
        expected_sha256: Option<&str>,
    ) -> Result<(), StorageError> {
        if expected_sha256.is_some_and(|sha| !is_lower_hex_sha256(sha)) {
            return Err(StorageError::InvalidValue {
                field: "checksum_expected",
            });
        }
        if !(MIN_CONNECTIONS..=MAX_CONNECTIONS).contains(&new.max_connections) {
            return Err(StorageError::InvalidValue {
                field: "max_connections",
            });
        }
        let id = id_text(new.id);
        let sealed = self.seal(SealColumn::OriginalUrl, &id, &new.original_url)?;
        let mut tx = self.storage.begin_write().await?;
        sqlx::query(
            "INSERT INTO downloads (id, state, source, priority, queue_position, \
             original_url_sealed, url_display, target_dir, requested_name, display_name, \
             collision_policy, max_connections, created_at_ms, updated_at_ms, \
             checksum_expected) \
             VALUES (?1, 'queued', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12, ?13)",
        )
        .bind(&id)
        .bind(new.source.as_str())
        .bind(priority_to_db(new.priority))
        .bind(new.queue_position)
        .bind(sealed)
        .bind(&new.url_display)
        .bind(&new.target_dir)
        .bind(new.requested_name.as_deref())
        .bind(&new.display_name)
        .bind(&new.collision_policy)
        .bind(i64::from(new.max_connections))
        .bind(new.created_at_ms)
        .bind(expected_sha256)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Forget all progress and the remote identity in one transaction: the segments are
    /// deleted, `bytes_committed` becomes 0, and the validators, total, staging path and
    /// staging file id are cleared, so the next probe starts from nothing (restart from
    /// zero). The state and the expected checksum are kept.
    ///
    /// # Errors
    /// [`StorageError::NotFound`] or a database error. On error nothing changes.
    pub async fn reset_for_restart(&self, id: Uuid, at_ms: i64) -> Result<(), StorageError> {
        let text = id_text(id);
        let mut tx = self.storage.begin_write().await?;
        let changed = sqlx::query(
            "UPDATE downloads SET bytes_committed = 0, total_size = NULL, etag = NULL, \
             etag_strong = NULL, last_modified = NULL, last_modified_strong = NULL, \
             part_path = NULL, part_file_id = NULL, updated_at_ms = ?2 WHERE id = ?1",
        )
        .bind(&text)
        .bind(at_ms)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed != 1 {
            return Err(StorageError::NotFound { id });
        }
        sqlx::query("DELETE FROM segments WHERE download_id = ?1")
            .bind(&text)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Persist the per-download connection cap (`1..=16`), so a limit changed at runtime
    /// survives an agent restart.
    ///
    /// # Errors
    /// [`StorageError::InvalidValue`] outside `1..=16` (nothing changes),
    /// [`StorageError::NotFound`] or a database error.
    pub async fn set_max_connections(
        &self,
        id: Uuid,
        value: u32,
        at_ms: i64,
    ) -> Result<(), StorageError> {
        if !(MIN_CONNECTIONS..=MAX_CONNECTIONS).contains(&value) {
            return Err(StorageError::InvalidValue {
                field: "max_connections",
            });
        }
        let mut tx = self.storage.begin_write().await?;
        let changed = sqlx::query(
            "UPDATE downloads SET max_connections = ?2, updated_at_ms = ?3 WHERE id = ?1",
        )
        .bind(id_text(id))
        .bind(i64::from(value))
        .bind(at_ms)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed != 1 {
            return Err(StorageError::NotFound { id });
        }
        tx.commit().await?;
        Ok(())
    }

    /// Record when a `retry_waiting` download is due (epoch milliseconds).
    ///
    /// # Errors
    /// [`StorageError::NotFound`] or a database error.
    pub async fn set_next_retry_at(
        &self,
        id: Uuid,
        next_retry_at_ms: i64,
        at_ms: i64,
    ) -> Result<(), StorageError> {
        let mut tx = self.storage.begin_write().await?;
        let changed = sqlx::query(
            "UPDATE downloads SET next_retry_at_ms = ?2, updated_at_ms = ?3 WHERE id = ?1",
        )
        .bind(id_text(id))
        .bind(next_retry_at_ms)
        .bind(at_ms)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed != 1 {
            return Err(StorageError::NotFound { id });
        }
        tx.commit().await?;
        Ok(())
    }

    /// Load one download (never unseals).
    ///
    /// # Errors
    /// [`StorageError::UnknownEnumValue`] and similar for corrupt rows, or a database error.
    pub async fn get(&self, id: Uuid) -> Result<Option<DownloadRecord>, StorageError> {
        let raw: Option<RawDownload> = sqlx::query_as(concat!(
            "SELECT ",
            download_columns!(),
            " FROM downloads WHERE id = ?1"
        ))
        .bind(id_text(id))
        .fetch_optional(self.storage.reader())
        .await?;
        raw.map(DownloadRecord::try_from).transpose()
    }

    /// List downloads newest first. `states = None` means all; `Some(&[])` matches nothing.
    /// `limit` is capped at 500 (`LIST_PAGE_MAX`).
    ///
    /// # Errors
    /// A database error, or a corrupt row.
    pub async fn list(
        &self,
        states: Option<&[DownloadState]>,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<DownloadRecord>, StorageError> {
        let filter = states
            .map(|s| serde_json::to_string(&s.iter().map(|x| x.as_str()).collect::<Vec<_>>()))
            .transpose()
            .map_err(|_| StorageError::InvalidValue { field: "states" })?;
        let raws: Vec<RawDownload> = sqlx::query_as(concat!(
            "SELECT ",
            download_columns!(),
            " FROM downloads \
             WHERE (?1 IS NULL OR state IN (SELECT value FROM json_each(?1))) \
             ORDER BY created_at_ms DESC, id ASC LIMIT ?2 OFFSET ?3"
        ))
        .bind(filter)
        .bind(i64::from(limit.min(LIST_PAGE_MAX)))
        .bind(i64::from(offset))
        .fetch_all(self.storage.reader())
        .await?;
        raws.into_iter().map(DownloadRecord::try_from).collect()
    }

    /// Compare-and-set the state and append the transition event in one transaction.
    ///
    /// # Errors
    /// [`StorageError::Conflict`] when the stored state is not `from`,
    /// [`StorageError::NotFound`], [`StorageError::InvalidValue`] for `Unknown` or for
    /// `to == Completed` (use [`Downloads::complete`]), or a database error. On error
    /// nothing changes.
    pub async fn transition(
        &self,
        id: Uuid,
        from: DownloadState,
        to: DownloadState,
        reason: Option<&str>,
        event: &NewEvent,
    ) -> Result<(), StorageError> {
        reject_unknown(from)?;
        reject_unknown(to)?;
        // Only `complete` records the final path and checksum, so only it may reach `completed`.
        if to == DownloadState::Completed {
            return Err(StorageError::InvalidValue { field: "state" });
        }
        let text = id_text(id);
        let mut tx = self.storage.begin_write().await?;
        let changed = sqlx::query(
            "UPDATE downloads SET state = ?1, state_reason = ?2, updated_at_ms = ?3 \
             WHERE id = ?4 AND state = ?5",
        )
        .bind(to.as_str())
        .bind(reason)
        .bind(event.at_ms)
        .bind(&text)
        .bind(from.as_str())
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed != 1 {
            return Err(conflict_or_missing(&mut tx, id, &text, from).await);
        }
        insert_event(&mut tx, &text, event, Some(from), Some(to)).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Mark a `finalizing` download `completed` and record the result, with an event.
    ///
    /// # Errors
    /// [`StorageError::Conflict`] when the download is not `finalizing`,
    /// [`StorageError::InvalidValue`] when `sha256_hex` is not 64 lowercase hex characters, or
    /// a database error.
    pub async fn complete(
        &self,
        id: Uuid,
        final_path: &str,
        sha256_hex: &str,
        length_verified: bool,
        at_ms: i64,
    ) -> Result<(), StorageError> {
        if !is_lower_hex_sha256(sha256_hex) {
            return Err(StorageError::InvalidValue {
                field: "checksum_actual",
            });
        }
        let text = id_text(id);
        let mut tx = self.storage.begin_write().await?;
        let changed = sqlx::query(
            "UPDATE downloads SET state = 'completed', state_reason = NULL, final_path = ?2, \
             checksum_actual = ?3, length_verified = ?4, completed_at_ms = ?5, updated_at_ms = ?5 \
             WHERE id = ?1 AND state = 'finalizing'",
        )
        .bind(&text)
        .bind(final_path)
        .bind(sha256_hex)
        .bind(length_verified)
        .bind(at_ms)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed != 1 {
            return Err(conflict_or_missing(&mut tx, id, &text, DownloadState::Finalizing).await);
        }
        let event = NewEvent::new(at_ms, "completed");
        let (from, to) = (DownloadState::Finalizing, DownloadState::Completed);
        insert_event(&mut tx, &text, &event, Some(from), Some(to)).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Record the destination chosen for the finalize step; only valid while `finalizing`.
    ///
    /// # Errors
    /// [`StorageError::Conflict`] when the download is not `finalizing`,
    /// [`StorageError::NotFound`] or a database error.
    pub async fn set_finalize_target(
        &self,
        id: Uuid,
        target: &str,
        at_ms: i64,
    ) -> Result<(), StorageError> {
        let text = id_text(id);
        let mut tx = self.storage.begin_write().await?;
        let changed = sqlx::query(
            "UPDATE downloads SET finalize_target = ?2, updated_at_ms = ?3 \
             WHERE id = ?1 AND state = 'finalizing'",
        )
        .bind(&text)
        .bind(target)
        .bind(at_ms)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed != 1 {
            return Err(conflict_or_missing(&mut tx, id, &text, DownloadState::Finalizing).await);
        }
        tx.commit().await?;
        Ok(())
    }

    /// Store probe results. See [`ProbeResult`] for which fields keep stored values.
    ///
    /// # Errors
    /// [`StorageError::Crypto`], [`StorageError::NotFound`] or a database error.
    pub async fn save_probe(
        &self,
        id: Uuid,
        probe: &ProbeResult,
        at_ms: i64,
    ) -> Result<(), StorageError> {
        let text = id_text(id);
        let final_sealed = probe
            .final_url
            .as_deref()
            .map(|url| self.seal(SealColumn::FinalUrl, &text, url))
            .transpose()?;
        let total = probe
            .total_size
            .map(|t| to_i64("total_size", t))
            .transpose()?;
        let mut tx = self.storage.begin_write().await?;
        let changed = sqlx::query(
            "UPDATE downloads SET transfer_mode = ?2, total_size = ?3, mime = ?4, \
             final_url_sealed = COALESCE(?5, final_url_sealed), file_name = COALESCE(?6, file_name), \
             part_path = COALESCE(?7, part_path), part_file_id = COALESCE(?8, part_file_id), \
             etag = ?9, etag_strong = ?10, last_modified = ?11, last_modified_strong = ?12, \
             updated_at_ms = ?13 WHERE id = ?1",
        )
        .bind(&text)
        .bind(probe.transfer_mode.map(|m| m.as_str()))
        .bind(total)
        .bind(probe.mime.as_deref())
        .bind(final_sealed)
        .bind(probe.file_name.as_deref())
        .bind(probe.part_path.as_deref())
        .bind(probe.part_file_id.as_deref())
        .bind(probe.etag.as_deref())
        .bind(probe.etag_strong)
        .bind(probe.last_modified.as_deref())
        .bind(probe.last_modified_strong)
        .bind(at_ms)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed != 1 {
            return Err(StorageError::NotFound { id });
        }
        tx.commit().await?;
        Ok(())
    }

    /// Delete a download; its segments and events cascade. Returns whether a row existed.
    ///
    /// # Errors
    /// A database error.
    pub async fn delete(&self, id: Uuid) -> Result<bool, StorageError> {
        let mut tx = self.storage.begin_write().await?;
        let changed = sqlx::query("DELETE FROM downloads WHERE id = ?1")
            .bind(id_text(id))
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        Ok(changed == 1)
    }

    /// Unseal the original URL.
    ///
    /// # Errors
    /// [`StorageError::NotFound`], or [`StorageError::Crypto`] when it cannot be unsealed.
    pub async fn original_url(&self, id: Uuid) -> Result<SecretUrl, StorageError> {
        let text = id_text(id);
        let blob: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT original_url_sealed FROM downloads WHERE id = ?1")
                .bind(&text)
                .fetch_optional(self.storage.reader())
                .await?;
        let blob = blob.ok_or(StorageError::NotFound { id })?;
        self.unseal(SealColumn::OriginalUrl, &text, &blob)
    }

    /// Unseal the final (post-redirect) URL, if one was recorded.
    ///
    /// # Errors
    /// [`StorageError::NotFound`], or [`StorageError::Crypto`] when it cannot be unsealed.
    pub async fn final_url(&self, id: Uuid) -> Result<Option<SecretUrl>, StorageError> {
        let text = id_text(id);
        let blob: Option<Option<Vec<u8>>> =
            sqlx::query_scalar("SELECT final_url_sealed FROM downloads WHERE id = ?1")
                .bind(&text)
                .fetch_optional(self.storage.reader())
                .await?;
        let blob = blob.ok_or(StorageError::NotFound { id })?;
        blob.map(|b| self.unseal(SealColumn::FinalUrl, &text, &b))
            .transpose()
    }
}

/// After a CAS matched no row: distinguish a missing row from a stale `from` state.
async fn conflict_or_missing(
    conn: &mut sqlx::SqliteConnection,
    id: Uuid,
    text: &str,
    expected: DownloadState,
) -> StorageError {
    let stored: Result<Option<String>, sqlx::Error> =
        sqlx::query_scalar("SELECT state FROM downloads WHERE id = ?1")
            .bind(text)
            .fetch_optional(conn)
            .await;
    match stored {
        Ok(None) => StorageError::NotFound { id },
        Ok(Some(state)) => StorageError::Conflict {
            id,
            expected,
            actual: parse_state(&state).ok(),
        },
        Err(err) => err.into(),
    }
}
