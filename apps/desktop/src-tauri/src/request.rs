//! Builds the agent requests for the webview-facing commands.
//!
//! Everything the webview supplies is validated here before any pipe connection is made:
//! ids must be UUIDs, blank optional fields are treated as absent. The agent re-validates
//! the rest (URL, folder, checksum) and stays the authority.

use forgedm_protocol::{
    DownloadIdRequest, Empty, EnqueueUrlRequest, LimitScope, MAX_ACTIVE_DOWNLOADS, MAX_CONNECTIONS,
    MIN_ACTIVE_DOWNLOADS, MIN_CONNECTIONS, MIN_RATE_LIMIT_BPS, PauseAllRequest, PauseRequest,
    Priority, RemoveRequest, RequestBody, RetryRequest, UpdateLimitsRequest, UpdatePriorityRequest,
};
use serde::Deserialize;
use serde_json::json;

use crate::error::{CommandError, CommandErrorKind};

fn invalid_id() -> CommandError {
    CommandError::new(
        CommandErrorKind::InvalidRequest,
        "That download identifier is not valid.",
    )
}

/// Parses `{"download_id": id, ..extra}` as the typed request `T`. The protocol's own
/// deserializer does the UUID check, so this crate needs no direct uuid dependency.
fn typed<T: serde::de::DeserializeOwned>(
    id: &str,
    extra: serde_json::Value,
) -> Result<T, CommandError> {
    let mut value = json!({ "download_id": id });
    if let (Some(target), serde_json::Value::Object(more)) = (value.as_object_mut(), extra) {
        target.extend(more);
    }
    serde_json::from_value(value).map_err(|_| invalid_id())
}

fn non_blank(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

/// `EnqueueUrl`: the URL is trimmed; blank folder or checksum mean "not given".
#[must_use]
pub fn enqueue_url(url: &str, target_dir: Option<&str>, sha256: Option<&str>) -> RequestBody {
    let mut request = EnqueueUrlRequest::new(url.trim());
    request.target_dir = non_blank(target_dir);
    request.expected_sha256 = non_blank(sha256);
    RequestBody::EnqueueUrl(request)
}

/// `GetDownload`.
///
/// # Errors
/// [`CommandErrorKind::InvalidRequest`] when `id` is not a UUID.
pub fn get_download(id: &str) -> Result<RequestBody, CommandError> {
    Ok(RequestBody::GetDownload(typed::<DownloadIdRequest>(
        id,
        json!({}),
    )?))
}

/// `Pause`.
///
/// # Errors
/// [`CommandErrorKind::InvalidRequest`] when `id` is not a UUID.
pub fn pause(id: &str, discard_progress: bool) -> Result<RequestBody, CommandError> {
    let extra = json!({ "discard_progress": discard_progress });
    Ok(RequestBody::Pause(typed::<PauseRequest>(id, extra)?))
}

/// `Resume`.
///
/// # Errors
/// [`CommandErrorKind::InvalidRequest`] when `id` is not a UUID.
pub fn resume(id: &str) -> Result<RequestBody, CommandError> {
    Ok(RequestBody::Resume(typed::<DownloadIdRequest>(
        id,
        json!({}),
    )?))
}

/// `Cancel`.
///
/// # Errors
/// [`CommandErrorKind::InvalidRequest`] when `id` is not a UUID.
pub fn cancel(id: &str) -> Result<RequestBody, CommandError> {
    Ok(RequestBody::Cancel(typed::<DownloadIdRequest>(
        id,
        json!({}),
    )?))
}

/// `Retry`.
///
/// # Errors
/// [`CommandErrorKind::InvalidRequest`] when `id` is not a UUID.
pub fn retry(id: &str, restart_from_zero: bool) -> Result<RequestBody, CommandError> {
    let extra = json!({ "restart_from_zero": restart_from_zero });
    Ok(RequestBody::Retry(typed::<RetryRequest>(id, extra)?))
}

/// `Remove`.
///
/// # Errors
/// [`CommandErrorKind::InvalidRequest`] when `id` is not a UUID.
pub fn remove(id: &str, delete_partial_file: bool) -> Result<RequestBody, CommandError> {
    let extra = json!({ "delete_partial_file": delete_partial_file });
    Ok(RequestBody::Remove(typed::<RemoveRequest>(id, extra)?))
}

/// `PauseAll`.
#[must_use]
pub fn pause_all(discard_progress: bool) -> RequestBody {
    RequestBody::PauseAll(PauseAllRequest { discard_progress })
}

/// `ResumeAll`.
#[must_use]
pub fn resume_all() -> RequestBody {
    RequestBody::ResumeAll(Empty {})
}

/// A rate cap as the webview states it: the word `"off"` or a bytes-per-second number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(try_from = "RateRepr")]
pub enum RateLimitChoice {
    /// No cap (sent as `null`).
    Off,
    /// A cap in bytes per second.
    BytesPerSecond(u64),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RateRepr {
    Word(String),
    Number(u64),
}

impl TryFrom<RateRepr> for RateLimitChoice {
    type Error = String;

    fn try_from(repr: RateRepr) -> Result<Self, Self::Error> {
        match repr {
            RateRepr::Word(word) if word == "off" => Ok(Self::Off),
            RateRepr::Word(_) => Err("a rate limit is a number of bytes or \"off\"".to_owned()),
            RateRepr::Number(bps) => Ok(Self::BytesPerSecond(bps)),
        }
    }
}

/// The fields of an `UpdateLimits` call; `None` leaves a limit unchanged.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LimitsChange {
    /// Connections per download (global default or one download), `1..=16`.
    pub max_connections: Option<u32>,
    /// Speed cap; [`RateLimitChoice::Off`] clears it.
    pub rate_limit: Option<RateLimitChoice>,
    /// Simultaneously active downloads, `1..=10`, global scope only.
    pub max_active_downloads: Option<u32>,
}

fn out_of_range(what: &str) -> CommandError {
    CommandError::new(
        CommandErrorKind::InvalidRequest,
        format!("The {what} is outside the allowed range."),
    )
}

fn check_range(value: Option<u32>, min: u32, max: u32, what: &str) -> Result<(), CommandError> {
    match value {
        Some(v) if !(min..=max).contains(&v) => Err(out_of_range(what)),
        _ => Ok(()),
    }
}

fn check_change(change: &LimitsChange, global: bool) -> Result<(), CommandError> {
    check_range(
        change.max_connections,
        MIN_CONNECTIONS,
        MAX_CONNECTIONS,
        "connection count",
    )?;
    if change.max_active_downloads.is_some() && !global {
        return Err(out_of_range("active download limit"));
    }
    check_range(
        change.max_active_downloads,
        MIN_ACTIVE_DOWNLOADS,
        MAX_ACTIVE_DOWNLOADS,
        "active download limit",
    )?;
    match change.rate_limit {
        Some(RateLimitChoice::BytesPerSecond(bps)) if bps < MIN_RATE_LIMIT_BPS => {
            Err(out_of_range("speed limit"))
        }
        _ => Ok(()),
    }
}

fn limit_scope(scope: &str, id: Option<&str>) -> Result<LimitScope, CommandError> {
    match (scope, id) {
        ("global", _) => Ok(LimitScope::Global),
        ("download", Some(id)) => {
            let target = typed::<DownloadIdRequest>(id, json!({}))?;
            Ok(LimitScope::Download {
                download_id: target.download_id,
            })
        }
        ("download", None) => Err(invalid_id()),
        _ => Err(CommandError::new(
            CommandErrorKind::InvalidRequest,
            "That limit scope is not valid.",
        )),
    }
}

/// `UpdateLimits` for `scope` (`"global"` or `"download"` plus its id).
///
/// # Errors
/// [`CommandErrorKind::InvalidRequest`] for an unknown scope, a malformed id, a value outside
/// the protocol range, or an active-download limit on a single download.
pub fn update_limits(
    scope: &str,
    download_id: Option<&str>,
    change: &LimitsChange,
) -> Result<RequestBody, CommandError> {
    let scope = limit_scope(scope, download_id)?;
    check_change(change, matches!(scope, LimitScope::Global))?;
    let mut request = UpdateLimitsRequest::new(scope);
    request.max_connections = change.max_connections;
    request.max_active_downloads = change.max_active_downloads;
    request.rate_limit_bps = change.rate_limit.map(|choice| match choice {
        RateLimitChoice::Off => None,
        RateLimitChoice::BytesPerSecond(bps) => Some(bps),
    });
    Ok(RequestBody::UpdateLimits(request))
}

/// Reads the global limits: a global `UpdateLimits` that changes nothing, as
/// `forgedm limits` does.
#[must_use]
pub fn get_limits() -> RequestBody {
    RequestBody::UpdateLimits(UpdateLimitsRequest::new(LimitScope::Global))
}

/// `UpdatePriority`; `priority` is `high`, `normal` or `low`.
///
/// # Errors
/// [`CommandErrorKind::InvalidRequest`] for a malformed id or an unknown priority.
pub fn update_priority(id: &str, priority: &str) -> Result<RequestBody, CommandError> {
    let target = typed::<DownloadIdRequest>(id, json!({}))?;
    let priority: Priority = serde_json::from_value(json!(priority)).map_err(|_| {
        CommandError::new(
            CommandErrorKind::InvalidRequest,
            "That priority is not valid.",
        )
    })?;
    Ok(RequestBody::UpdatePriority(UpdatePriorityRequest {
        download_id: target.download_id,
        priority,
    }))
}
