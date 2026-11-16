//! Tauri commands exposed to the webview. Each is ACL-gated through the app manifest in
//! `build.rs` and granted only by `capabilities/main-window.json` (ADR-022).

use forgedm_protocol::{
    AgentStatusInfo, DownloadDetail, EnqueueUrlResult, ListDownloadsResult, PauseAllResult,
    ResumeAllResult, SnapshotResult, StateResult, UpdateLimitsResult,
};
use std::sync::Mutex;

use tauri::State;

use crate::agent_client::{self, AgentLocator};
use crate::error::CommandError;
use crate::request::{LimitsChange, RateLimitChoice};
use crate::{engine, open_folder, request};

/// The registered command names; must equal the `build.rs` manifest list and the
/// `generate_handler!` entries (checked by `manifest_matches_registered_commands`).
pub const COMMAND_NAMES: [&str; 18] = [
    "agent_status",
    "list_downloads",
    "get_snapshot",
    "get_download",
    "enqueue_url",
    "pause",
    "resume",
    "cancel",
    "retry",
    "remove",
    "start_engine",
    "stop_engine",
    "show_in_folder",
    "pause_all",
    "resume_all",
    "update_limits",
    "update_priority",
    "get_limits",
];

/// Builds the locator; replaceable so tests can simulate a startup failure.
pub type Resolver = fn() -> Result<AgentLocator, CommandError>;

/// Managed state: resolves the agent locator lazily and caches only a success, so a
/// missing agent executable or a transient lookup failure recovers on a later call.
#[derive(Debug)]
pub struct AgentTarget {
    resolver: Resolver,
    cached: Mutex<Option<AgentLocator>>,
}

impl AgentTarget {
    /// A target that resolves the locator on first successful use.
    #[must_use]
    pub fn new(resolver: Resolver) -> Self {
        Self {
            resolver,
            cached: Mutex::new(None),
        }
    }

    /// The cached locator, resolving it now when none is cached yet.
    ///
    /// # Errors
    /// The resolver's failure; nothing is cached in that case.
    pub fn locator(&self) -> Result<AgentLocator, CommandError> {
        let mut guard = self
            .cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(found) = guard.as_ref() {
            return Ok(found.clone());
        }
        let resolved = (self.resolver)()?;
        *guard = Some(resolved.clone());
        Ok(resolved)
    }
}

/// Current agent status, fetched over a fresh pipe connection.
///
/// # Errors
/// [`CommandError`] (`agent_unavailable` when the pipe is absent).
#[tauri::command]
pub async fn agent_status(target: State<'_, AgentTarget>) -> Result<AgentStatusInfo, CommandError> {
    agent_client::fetch_status(&target.locator()?).await
}

/// The download list (first page), fetched over a fresh pipe connection.
///
/// # Errors
/// [`CommandError`] (`agent_unavailable` when the pipe is absent).
#[tauri::command]
pub async fn list_downloads(
    target: State<'_, AgentTarget>,
) -> Result<ListDownloadsResult, CommandError> {
    agent_client::fetch_downloads(&target.locator()?).await
}

/// Agent status and the download list in one consistent answer (polled once a second).
///
/// # Errors
/// [`CommandError`] (`agent_unavailable` when the pipe is absent).
#[tauri::command(rename_all = "snake_case")]
pub async fn get_snapshot(target: State<'_, AgentTarget>) -> Result<SnapshotResult, CommandError> {
    agent_client::fetch_snapshot(&target.locator()?).await
}

/// One download's detail.
///
/// # Errors
/// [`CommandError`] (`invalid_request` for a malformed id, `agent_error` when unknown).
#[tauri::command(rename_all = "snake_case")]
pub async fn get_download(
    target: State<'_, AgentTarget>,
    id: String,
) -> Result<DownloadDetail, CommandError> {
    let body = request::get_download(&id)?;
    agent_client::fetch_download(&target.locator()?, body).await
}

/// Adds a download. `target_dir` empty or absent means the agent's default folder.
///
/// # Errors
/// [`CommandError`] with a user-facing message when the agent rejects the request.
#[tauri::command(rename_all = "snake_case")]
pub async fn enqueue_url(
    target: State<'_, AgentTarget>,
    url: String,
    target_dir: Option<String>,
    sha256: Option<String>,
) -> Result<EnqueueUrlResult, CommandError> {
    let body = request::enqueue_url(&url, target_dir.as_deref(), sha256.as_deref());
    agent_client::enqueue(&target.locator()?, body).await
}

/// Pauses a download; `discard_progress` allows pausing a non-resumable one.
///
/// # Errors
/// [`CommandError`] (`invalid_request` for a malformed id).
#[tauri::command(rename_all = "snake_case")]
pub async fn pause(
    target: State<'_, AgentTarget>,
    id: String,
    discard_progress: Option<bool>,
) -> Result<StateResult, CommandError> {
    let body = request::pause(&id, discard_progress.unwrap_or(false))?;
    agent_client::change_state(&target.locator()?, body).await
}

/// Resumes a paused or interrupted download.
///
/// # Errors
/// [`CommandError`] (`invalid_request` for a malformed id).
#[tauri::command(rename_all = "snake_case")]
pub async fn resume(
    target: State<'_, AgentTarget>,
    id: String,
) -> Result<StateResult, CommandError> {
    agent_client::change_state(&target.locator()?, request::resume(&id)?).await
}

/// Cancels a download (the partial file is kept).
///
/// # Errors
/// [`CommandError`] (`invalid_request` for a malformed id).
#[tauri::command(rename_all = "snake_case")]
pub async fn cancel(
    target: State<'_, AgentTarget>,
    id: String,
) -> Result<StateResult, CommandError> {
    agent_client::change_state(&target.locator()?, request::cancel(&id)?).await
}

/// Retries a failed, cancelled or blocked download.
///
/// # Errors
/// [`CommandError`] (`invalid_request` for a malformed id).
#[tauri::command(rename_all = "snake_case")]
pub async fn retry(
    target: State<'_, AgentTarget>,
    id: String,
    restart_from_zero: Option<bool>,
) -> Result<StateResult, CommandError> {
    let body = request::retry(&id, restart_from_zero.unwrap_or(false))?;
    agent_client::change_state(&target.locator()?, body).await
}

/// Removes a download from the list; `delete_partial` also deletes its `.part` file.
///
/// # Errors
/// [`CommandError`] (`invalid_request` for a malformed id).
#[tauri::command(rename_all = "snake_case")]
pub async fn remove(
    target: State<'_, AgentTarget>,
    id: String,
    delete_partial: bool,
) -> Result<(), CommandError> {
    let body = request::remove(&id, delete_partial)?;
    agent_client::remove(&target.locator()?, body).await
}

/// Starts the background engine when it is not running and returns its status.
///
/// # Errors
/// [`CommandError`] (`agent_unavailable` when the engine is missing or does not start).
#[tauri::command(rename_all = "snake_case")]
pub async fn start_engine(target: State<'_, AgentTarget>) -> Result<AgentStatusInfo, CommandError> {
    engine::start_engine(&target.locator()?, None).await
}

/// Asks the background engine to pause its downloads and exit.
///
/// # Errors
/// [`CommandError`] (`agent_unavailable` when it is not running).
#[tauri::command(rename_all = "snake_case")]
pub async fn stop_engine(target: State<'_, AgentTarget>) -> Result<(), CommandError> {
    engine::stop_engine(&target.locator()?).await
}

/// Reveals a completed download in Explorer. The path comes from the agent, never from
/// the webview.
///
/// # Errors
/// [`CommandError`] (`invalid_request` when the download is not completed or the file is gone).
#[tauri::command(rename_all = "snake_case")]
pub async fn show_in_folder(
    target: State<'_, AgentTarget>,
    id: String,
) -> Result<(), CommandError> {
    let body = request::get_download(&id)?;
    let detail = agent_client::fetch_download(&target.locator()?, body).await?;
    let plan = open_folder::plan_launch(&detail.summary, &open_folder::system_root()?)?;
    open_folder::launch(&plan)
}

/// Pauses every eligible download; active non-resumable ones are skipped unless
/// `discard_progress`.
///
/// # Errors
/// [`CommandError`] (`agent_unavailable` when the engine is not running).
#[tauri::command(rename_all = "snake_case")]
pub async fn pause_all(
    target: State<'_, AgentTarget>,
    discard_progress: Option<bool>,
) -> Result<PauseAllResult, CommandError> {
    let body = request::pause_all(discard_progress.unwrap_or(false));
    agent_client::pause_all(&target.locator()?, body).await
}

/// Resumes every paused or interrupted download.
///
/// # Errors
/// [`CommandError`] (`agent_unavailable` when the engine is not running).
#[tauri::command(rename_all = "snake_case")]
pub async fn resume_all(target: State<'_, AgentTarget>) -> Result<ResumeAllResult, CommandError> {
    agent_client::resume_all(&target.locator()?, request::resume_all()).await
}

/// Changes limits and returns the ones now in force. `scope` is `global` or `download`
/// (which needs `download_id`); `rate_limit` is `"off"` or bytes per second.
///
/// # Errors
/// [`CommandError`] (`invalid_request` for a value outside the protocol range).
#[tauri::command(rename_all = "snake_case")]
pub async fn update_limits(
    target: State<'_, AgentTarget>,
    scope: String,
    download_id: Option<String>,
    max_connections: Option<u32>,
    rate_limit: Option<RateLimitChoice>,
    max_active_downloads: Option<u32>,
) -> Result<UpdateLimitsResult, CommandError> {
    let change = LimitsChange {
        max_connections,
        rate_limit,
        max_active_downloads,
    };
    let body = request::update_limits(&scope, download_id.as_deref(), &change)?;
    agent_client::update_limits(&target.locator()?, body).await
}

/// The global limits in force, read without changing anything.
///
/// # Errors
/// [`CommandError`] (`agent_unavailable` when the engine is not running).
#[tauri::command(rename_all = "snake_case")]
pub async fn get_limits(
    target: State<'_, AgentTarget>,
) -> Result<UpdateLimitsResult, CommandError> {
    agent_client::update_limits(&target.locator()?, request::get_limits()).await
}

/// Sets a download's queue priority (`high`, `normal` or `low`).
///
/// # Errors
/// [`CommandError`] (`invalid_request` for a malformed id or priority).
#[tauri::command(rename_all = "snake_case")]
pub async fn update_priority(
    target: State<'_, AgentTarget>,
    id: String,
    priority: String,
) -> Result<(), CommandError> {
    let body = request::update_priority(&id, &priority)?;
    agent_client::update_priority(&target.locator()?, body).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::CommandErrorKind;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static CALLS: AtomicUsize = AtomicUsize::new(0);

    fn fails_once_then_succeeds() -> Result<AgentLocator, CommandError> {
        if CALLS.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(CommandError::new(
                CommandErrorKind::AgentUnavailable,
                "missing",
            ))
        } else {
            Ok(AgentLocator::new("pipe", "agent.exe"))
        }
    }

    #[test]
    fn locator_failure_is_not_cached_and_success_is() {
        let target = AgentTarget::new(fails_once_then_succeeds);
        assert!(target.locator().is_err());
        assert!(target.locator().is_ok(), "recovers after the first failure");
        assert!(target.locator().is_ok());
        assert_eq!(CALLS.load(Ordering::SeqCst), 2, "success is cached");
    }
}
