//! Agent-wide limits kept in the `settings` table (PLAN S1.13): how many downloads run at
//! once, the global rate limit and the default connections per download.
//!
//! They are saved before they are applied, and read back (and re-validated: the database is
//! not trusted blindly) when the agent starts.

use forgedm_protocol::{MAX_CONNECTIONS, MIN_CONNECTIONS};
use forgedm_storage::{SettingKey, Storage, StorageError};
use serde_json::Value;

/// Most downloads that may run at once (A.9).
pub const MAX_ACTIVE_CEILING: u32 = 10;
/// Smallest rate limit accepted, in bytes per second (A.3).
pub const MIN_RATE_LIMIT_BPS: u64 = 1024;
/// Default connections per download when nothing is stored (A.9 `CONNS_PER_DOWNLOAD`).
pub const DEFAULT_CONNECTIONS: u32 = 8;

/// A change to the agent-wide limits; `None` leaves a limit alone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GlobalPatch {
    /// Default connections per download, `1..=16`.
    pub max_connections: Option<u32>,
    /// Rate limit in bytes per second: `Some(None)` is unlimited.
    pub rate_limit_bps: Option<Option<u64>>,
    /// Downloads that run at once, `1..=10`.
    pub max_active_downloads: Option<u32>,
}

impl GlobalPatch {
    /// Whether the patch changes nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// What was stored; every field is `None` when nothing (valid) was stored.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SavedLimits {
    /// Default connections per download.
    pub max_connections: Option<u32>,
    /// Rate limit; `Some(None)` is stored "unlimited".
    pub rate_limit_bps: Option<Option<u64>>,
    /// Downloads that run at once.
    pub max_active_downloads: Option<u32>,
}

fn in_range(value: &Value, min: u32, max: u32) -> Option<u32> {
    let number = u32::try_from(value.as_u64()?).ok()?;
    (min..=max).contains(&number).then_some(number)
}

fn rate_of(value: &Value) -> Option<Option<u64>> {
    if value.is_null() {
        return Some(None);
    }
    value
        .as_u64()
        .filter(|bps| *bps >= MIN_RATE_LIMIT_BPS)
        .map(Some)
}

/// Reads the stored limits. A value that is missing, malformed or out of range is ignored
/// (with a warning), never applied.
///
/// # Errors
/// [`StorageError`] when the database cannot be read.
pub async fn load(storage: &Storage) -> Result<SavedLimits, StorageError> {
    let settings = storage.settings();
    let max_connections = settings.get(SettingKey::GlobalMaxConnections).await?;
    let rate = settings.get(SettingKey::GlobalRateLimitBps).await?;
    let active = settings.get(SettingKey::MaxActiveDownloads).await?;
    let saved = SavedLimits {
        max_connections: max_connections
            .as_ref()
            .and_then(|v| in_range(v, MIN_CONNECTIONS, MAX_CONNECTIONS)),
        rate_limit_bps: rate.as_ref().and_then(rate_of),
        max_active_downloads: active
            .as_ref()
            .and_then(|v| in_range(v, 1, MAX_ACTIVE_CEILING)),
    };
    for (key, stored, usable) in [
        (
            "global_max_connections",
            max_connections.is_some(),
            saved.max_connections.is_some(),
        ),
        (
            "global_rate_limit_bps",
            rate.is_some(),
            saved.rate_limit_bps.is_some(),
        ),
        (
            "max_active_downloads",
            active.is_some(),
            saved.max_active_downloads.is_some(),
        ),
    ] {
        if stored && !usable {
            tracing::warn!(key, "ignoring an invalid stored limit");
        }
    }
    Ok(saved)
}

/// Stores the limits `patch` changes, one setting each.
///
/// # Errors
/// [`StorageError`] when a write fails (earlier writes of the same call stay).
pub async fn save(storage: &Storage, patch: &GlobalPatch, at_ms: i64) -> Result<(), StorageError> {
    let settings = storage.settings();
    if let Some(value) = patch.max_connections {
        settings
            .put(SettingKey::GlobalMaxConnections, &Value::from(value), at_ms)
            .await?;
    }
    if let Some(limit) = patch.rate_limit_bps {
        let value = limit.map_or(Value::Null, Value::from);
        settings
            .put(SettingKey::GlobalRateLimitBps, &value, at_ms)
            .await?;
    }
    if let Some(value) = patch.max_active_downloads {
        settings
            .put(SettingKey::MaxActiveDownloads, &Value::from(value), at_ms)
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn stored_values_are_checked_before_use() {
        assert_eq!(in_range(&json!(8), 1, 16), Some(8));
        assert_eq!(in_range(&json!(0), 1, 16), None);
        assert_eq!(in_range(&json!(17), 1, 16), None);
        assert_eq!(in_range(&json!("8"), 1, 16), None);
        assert_eq!(in_range(&json!(-1), 1, 16), None);
        assert_eq!(rate_of(&Value::Null), Some(None));
        assert_eq!(rate_of(&json!(2048)), Some(Some(2048)));
        assert_eq!(rate_of(&json!(10)), None);
        assert_eq!(rate_of(&json!(1.5)), None);
    }

    #[test]
    fn an_empty_patch_changes_nothing() {
        assert!(GlobalPatch::default().is_empty());
        let patch = GlobalPatch {
            rate_limit_bps: Some(None),
            ..GlobalPatch::default()
        };
        assert!(!patch.is_empty());
    }
}
