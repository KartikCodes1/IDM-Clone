//! Builds the agent requests for the webview-facing commands.
//!
//! Everything the webview supplies is validated here before any pipe connection is made:
//! ids must be UUIDs, blank optional fields are treated as absent. The agent re-validates
//! the rest (URL, folder, checksum) and stays the authority.

use forgedm_protocol::{
    DownloadIdRequest, EnqueueUrlRequest, PauseRequest, RemoveRequest, RequestBody, RetryRequest,
};
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
