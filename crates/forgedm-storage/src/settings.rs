//! Key/value settings with a closed key set.

use serde_json::Value;

use crate::SETTING_VALUE_MAX_BYTES;
use crate::db::Storage;
use crate::error::StorageError;

/// The closed set of setting keys (Phase 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingKey {
    /// Maximum concurrently active downloads (expected 1..=10; range enforced by the
    /// protocol layer, not by storage).
    MaxActiveDownloads,
    /// Global default connections per download (expected 1..=16; not enforced by storage).
    GlobalMaxConnections,
    /// Global rate limit in bytes per second (`null` is unlimited; not enforced by storage).
    GlobalRateLimitBps,
}

impl SettingKey {
    /// The stored key string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MaxActiveDownloads => "max_active_downloads",
            Self::GlobalMaxConnections => "global_max_connections",
            Self::GlobalRateLimitBps => "global_rate_limit_bps",
        }
    }
}

/// Settings repository, from [`Storage::settings`].
#[derive(Debug, Clone)]
pub struct Settings {
    storage: Storage,
}

impl Settings {
    pub(crate) fn new(storage: Storage) -> Self {
        Self { storage }
    }

    /// Read a setting.
    ///
    /// # Errors
    /// [`StorageError`] on a database failure or a stored value that is not JSON.
    pub async fn get(&self, key: SettingKey) -> Result<Option<Value>, StorageError> {
        let text: Option<String> =
            sqlx::query_scalar("SELECT value_json FROM settings WHERE key = ?1")
                .bind(key.as_str())
                .fetch_optional(self.storage.reader())
                .await?;
        text.map(|t| {
            serde_json::from_str(&t).map_err(|_| StorageError::InvalidValue {
                field: "value_json",
            })
        })
        .transpose()
    }

    /// Insert or replace a setting (at most 16 KiB serialized). Storage checks only the
    /// size; type and range validation belong to the caller.
    ///
    /// # Errors
    /// [`StorageError::ValueTooLarge`] or a database error.
    pub async fn put(
        &self,
        key: SettingKey,
        value: &Value,
        at_ms: i64,
    ) -> Result<(), StorageError> {
        let text = value.to_string();
        if text.len() > SETTING_VALUE_MAX_BYTES {
            return Err(StorageError::ValueTooLarge {
                what: "setting value",
                max: SETTING_VALUE_MAX_BYTES,
            });
        }
        let mut tx = self.storage.begin_write().await?;
        sqlx::query(
            "INSERT INTO settings (key, value_json, updated_at_ms) VALUES (?1, ?2, ?3) \
             ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json, \
             updated_at_ms = excluded.updated_at_ms",
        )
        .bind(key.as_str())
        .bind(text)
        .bind(at_ms)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }
}
